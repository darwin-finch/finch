fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn main() {
    let mut vm = finch::coforth::Forth::new();

    println!("=== prove-all ===");
    let out = vm.exec("prove-all").unwrap();
    println!("{}", strip_ansi(&out));

    println!("\n=== prove\" square\" ===");
    let out = vm.exec(r#"prove" square""#).unwrap();
    println!("{}", strip_ansi(&out));

    println!("\n=== prove\" fib\" ===");
    let out = vm.exec(r#"prove" fib""#).unwrap();
    println!("{}", strip_ansi(&out));

    println!("\n=== user defines + proves own word ===");
    vm.exec(": double dup + ;").unwrap();
    vm.exec(": test:double  5 double 10 = assert  0 double 0 = assert  -3 double -6 = assert ;")
        .unwrap();
    let out = vm.exec(r#"prove" double""#).unwrap();
    println!("{}", strip_ansi(&out));

    println!("\n=== broken word caught by prove ===");
    vm.exec(": broken  0 assert ;").unwrap();
    vm.exec(": test:broken  broken ;").unwrap();
    let out = vm.exec(r#"prove" broken""#).unwrap();
    println!("{}", strip_ansi(&out));

    println!("\n=== see shows proof hint ===");
    let out = vm.exec(r#"see" square""#).unwrap();
    println!("{}", strip_ansi(&out));
}
