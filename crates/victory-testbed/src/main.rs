//! `testbed hle list | hle call <dll> <name> [args] | alu <hex words> --in a,b,c,d [--hardware]`

use std::process::ExitCode;


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
                .map(|s| victory_testbed::parse_num(s).map(|v| v as usize))
                .collect::<Result<Vec<_>, _>>()?;
            // SAFETY: the owner picks the export; the help says to call only ones that need no title.
            let r = unsafe { victory_testbed::call_export(dll, name, &ints)? };
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
                        inputs = victory_testbed::parse_inputs(v)?;
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
