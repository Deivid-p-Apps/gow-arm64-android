#[test]
fn tmp_cc() {
    use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, Formatter, NasmFormatter};
    let cases: &[(&[u8], &str)] = &[
        (&[0x74,0x00], "je"),
        (&[0x75,0x00], "jne"),
        (&[0x72,0x00], "jb"),
        (&[0x73,0x00], "jae"),
        (&[0x7c,0x00], "jl"),
        (&[0x7f,0x00], "jg"),
    ];
    for (b, name) in cases {
        let mut d = Decoder::with_ip(64, b, 0, DecoderOptions::NONE);
        let mut i = Instruction::default();
        d.decode_out(&mut i);
        let mut s = String::new();
        NasmFormatter::new().format(&i, &mut s);
        println!("{}: mnemonic={:?} cc={} text={}", name, i.mnemonic(), i.condition_code() as u32, s);
    }
    assert!(false, "printed");
}
