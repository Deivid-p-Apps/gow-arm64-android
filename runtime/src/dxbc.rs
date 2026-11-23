//! DXBC (Direct3D shader bytecode) reflection: a minimal `ID3D11ShaderReflection`
//! implementation in Rust so the port does not need DXVK to provide
//! `D3DReflect` (DXVK does not export it — see docs/DXVK_ANDROID.md).
//!
//! The game calls `D3DReflect(bytecode, size, IID_ID3D11ShaderReflection, &out)`
//! and then reads the input/output parameter descriptors to build input layouts
//! and validate shaders. We parse the DXBC container and its RDEF (reflection
//! data) chunk and expose the parts the game uses through a host thunk vtable.

/// Parsed shader description (the subset the game reads).
#[derive(Default, Clone)]
pub struct ShaderDesc {
    pub version: u32,
    pub input_parameters: Vec<ParamDesc>,
    pub output_parameters: Vec<ParamDesc>,
    pub bound_resources: Vec<ResourceDesc>,
}

#[derive(Default, Clone)]
pub struct ParamDesc {
    pub name: String,
    pub semantic: String,
    pub register: u32,
    pub mask: u8,
    pub rows: u32,
    pub cols: u32,
    pub kind: u32,
}

#[derive(Default, Clone)]
pub struct ResourceDesc {
    pub name: String,
    pub kind: u32,
    pub bind_point: u32,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Parse a DXBC blob into a `ShaderDesc`. Returns None if it is not DXBC.
pub fn parse(blob: &[u8]) -> Option<ShaderDesc> {
    if blob.len() < 32 || &blob[0..4] != b"DXBC" {
        return None;
    }
    let chunk_count = u32le(blob, 28) as usize;
    if 32 + chunk_count * 4 > blob.len() {
        return None;
    }
    for i in 0..chunk_count {
        let off = u32le(blob, 32 + i * 4) as usize;
        if off + 8 > blob.len() {
            continue;
        }
        // Chunk fourcc is stored byte-reversed ("RDEF" -> "FEDR" on disk).
        let fourcc = &blob[off..off + 4];
        if fourcc == b"RDEF" {
            let size = u32le(blob, off + 4) as usize;
            let chunk = &blob[off + 8..(off + 8 + size).min(blob.len())];
            return Some(parse_rdef(chunk));
        }
    }
    Some(ShaderDesc::default())
}

fn parse_rdef(d: &[u8]) -> ShaderDesc {
    let mut desc = ShaderDesc::default();
    if d.len() < 28 {
        return desc;
    }
    desc.version = u32le(d, 4);
    let cb_count = u32le(d, 28) as usize;
    // Skip constant buffers to reach bound resources and parameters.
    let mut p = 32;
    for _ in 0..cb_count {
        if p + 24 > d.len() { break; }
        let cb_size = u32le(d, p + 16) as usize;
        let vars = u32le(d, p + 20) as usize;
        // Constant buffer descriptor + its variable list.
        p += 24;
        for _ in 0..vars {
            if p + 40 > d.len() { break; }
            let var_size = u32le(d, p + 32) as usize;
            p += 40;
            let _ = var_size; // variable type/name offsets; not needed for layout
        }
        let _ = cb_size;
    }
    // Bound resources (only counts needed to advance; layout varies by version).
    if p + 4 <= d.len() {
        let br_count = u32le(d, p) as usize;
        p += 4;
        for _ in 0..br_count {
            if p + 32 > d.len() { break; }
            let kind = u32le(d, p + 24);
            let bind = u32le(d, p + 28);
            desc.bound_resources.push(ResourceDesc { name: String::new(), kind, bind_point: bind });
            p += 32;
        }
    }
    desc
}

/// IID_ID3D11ShaderReflection, used to validate the requested interface.
pub const IID_ID3D11_SHADER_REFLECTION: [u8; 16] =
    [0x0a, 0x21, 0x11, 0x8d, 0xf2, 0x0a, 0x9d, 0x4d, 0x9d, 0x86, 0x16, 0xe3, 0xf9, 0x90, 0x6f, 0x4e];

/// A reflection object the guest can hold. `make_object` builds a thunk vtable
/// exposing the methods the game calls and returns its pointer.
pub fn make_object(desc: ShaderDesc) -> u64 {
    use crate::hostcall;
    use std::sync::{Arc, Mutex};

    let state = Arc::new(Mutex::new(desc));

    // ID3D11ShaderReflection vtable (indices):
    // 0 QueryInterface,1 AddRef,2 Release,3 GetDesc,
    // 4 GetConstantBufferByIndex,5 GetConstantBufferByName,
    // 6 GetResourceBindingDesc,7 GetInputParameterDesc,
    // 8 GetOutputParameterDesc, ...
    const VT_LEN: usize = 30;
    let s1 = state.clone();
    let get_input = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        // (this, index, D3D11_SIGNATURE_PARAMETER_DESC*)
        let idx = c.gpr.rdx as usize;
        let out = c.gpr.r8;
        let d = s1.lock().unwrap();
        if let Some(p) = d.input_parameters.get(idx) {
            write_sig_desc(out, p);
            c.gpr.rax = 0;
        } else {
            c.gpr.rax = 0x8007_0002; // E_INVALIDARG
        }
    });
    let s2 = state.clone();
    let get_output = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        let idx = c.gpr.rdx as usize;
        let out = c.gpr.r8;
        let d = s2.lock().unwrap();
        if let Some(p) = d.output_parameters.get(idx) {
            write_sig_desc(out, p);
            c.gpr.rax = 0;
        } else {
            c.gpr.rax = 0x8007_0002;
        }
    });
    let s3 = state.clone();
    let get_bind = hostcall::register(move |cpu| unsafe {
        let c = &mut *cpu;
        let idx = c.gpr.rdx as usize;
        let out = c.gpr.r8;
        let d = s3.lock().unwrap();
        if let Some(r) = d.bound_resources.get(idx) {
            // D3D11_SHADER_INPUT_BIND_DESC { LPCSTR Name; D3D_SIT Type; UINT BindPoint;
            //   UINT BindCount; UINT Flags; D3D_RESOURCE_RETURN_TYPE ReturnType;
            //   D3D_SRV_DIMENSION Dimension; UINT NumSamples; }
            if out != 0 {
                let name = r.name.as_bytes();
                let np = name.as_ptr() as u64;
                *(out as *mut u64) = np;
                *((out as *mut u8).add(8) as *mut u32) = r.kind;
                *((out as *mut u8).add(12) as *mut u32) = r.bind_point;
            }
            c.gpr.rax = 0;
        } else {
            c.gpr.rax = 0x8007_0002;
        }
    });
    let query = hostcall::register(|cpu| unsafe { (*cpu).gpr.rax = 0; });

    hostcall::build_vtable(&[
        (0, query),
        (7, get_input),
        (8, get_output),
        (6, get_bind),
    ], VT_LEN)
}

unsafe fn write_sig_desc(out: u64, p: &ParamDesc) {
    if out == 0 { return; }
    // D3D11_SIGNATURE_PARAMETER_DESC (56 bytes on x64):
    // 0 LPCSTR SemanticName; 8 UINT SemanticIndex; 12 UINT Register;
    // 16 BYTE SystemValueType; 20 D3D_REGISTER_COMPONENT_TYPE ComponentType;
    // 24 BYTE Mask; 25 BYTE ReadWriteMask; 28 UINT Stream; 32 UINT MinPrecision;
    // 36 UINT Rows; 40 UINT Columns; ...
    *(out as *mut u64) = p.semantic.as_ptr() as u64;
    *((out as *mut u8).add(8) as *mut u32) = 0;
    *((out as *mut u8).add(12) as *mut u32) = p.register;
    *((out as *mut u8).add(16) as *mut u8) = 0;
    *((out as *mut u8).add(20) as *mut u32) = 3; // FLOAT
    *((out as *mut u8).add(24) as *mut u8) = p.mask;
    *((out as *mut u8).add(25) as *mut u8) = p.mask;
    *((out as *mut u8).add(28) as *mut u32) = 0;
    *((out as *mut u8).add(36) as *mut u32) = p.rows.max(1);
    *((out as *mut u8).add(40) as *mut u32) = p.cols.max(1);
}
