//! `testbed hle list | hle call <dll> <name> [args] | alu <hex words> --in a,b,c,d [--hardware]`

use std::process::ExitCode;

fn num(s: &str) -> Result<u64, String> {
    let s = s.trim();
    match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("{s}: {e}"))
}

fn run(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["hle", "list"] => {
            for (module, dlls, name) in victory_testbed::list_exports() {
                println!("{module}\t{}\t{name}", dlls.join(","));
            }
        }
        ["hle", "call", dll, name, rest @ ..] => {
            let ints = rest
                .iter()
                .map(|s| num(s).map(|v| v as usize))
                .collect::<Result<Vec<_>, _>>()?;
            let r = victory_testbed::call_export(dll, name, &ints)?;
            println!("{r} (0x{r:x})");
        }
        #[cfg(windows)]
        ["alu", rest @ ..] => {
            let mut words = Vec::new();
            let mut inputs = [0u32; 4];
            let mut warp = true;
            let mut it = rest.iter();
            while let Some(&s) = it.next() {
                match s {
                    "--in" => {
                        let v = it.next().ok_or("--in needs a,b,c,d")?;
                        for (i, p) in v.split(',').take(4).enumerate() {
                            inputs[i] = num(p)? as u32;
                        }
                    }
                    "--hardware" => warp = false,
                    w => words.push(
                        u32::from_str_radix(w.trim_start_matches("0x"), 16)
                            .map_err(|e| format!("{w}: {e}"))?,
                    ),
                }
            }
            let r = victory_testbed::run_alu(&words, inputs, warp)?;
            println!("{r} (0x{r:08x}) on {}", if warp { "WARP" } else { "hardware" });
        }
        _ => return Err("usage: testbed hle list | hle call <dll> <name> [args] | alu <hex words> --in a,b,c,d [--hardware]".into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
