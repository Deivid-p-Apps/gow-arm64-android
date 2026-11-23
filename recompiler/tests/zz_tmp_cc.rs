#[test]
fn tmp_ops2() {
    use iced_x86::{Decoder, DecoderOptions, Instruction, Formatter, NasmFormatter};
    let cases: &[(&[u8], &str)] = &[
        (&[0xDD,0xDC], "fstpnce st4"),
        (&[0xDF,0xED], "fucomip st5"),
        (&[0xDB,0xF3], "fcomi st3"),
        (&[0xD9,0xC9], "fxch (none)"),
        (&[0xD9,0xCB], "fxch st3"),
        (&[0xDD,0xD3], "fst st3"),
        (&[0xDD,0xDB], "fstp st3"),
        (&[0xDD,0x23], "frstor [rbx]"),
        (&[0xD9,0x67,0x10], "fnstenv [rdi+0x10]"),
        (&[0xD9,0x27], "fldenv [rdi]"),
    ];
    for (b, name) in cases {
        let mut d = Decoder::with_ip(64, b, 0x1400000, DecoderOptions::NONE);
        let mut i = Instruction::default();
        d.decode_out(&mut i);
        let mut s = String::new();
        NasmFormatter::new().format(&i, &mut s);
        println!("{name} => {:?} | {s} | op0={:?} op1={:?}", i.mnemonic(), i.op0_register(), i.op1_register());
    }
    assert!(false, "printed");
}
