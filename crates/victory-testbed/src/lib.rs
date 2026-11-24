//! Headless testbed for the translation layer: call an HLE export by DLL and
//! name, or run a hand-written GFX10 sequence through the shader translation
//! and execute it on WARP. No game data, no title run.

use victory_hle::registry;

#[cfg(windows)]
pub use victory_shader::emit::testbed::run_alu;

/// Every export as `(module, dll patterns, name)`.
pub fn list_exports() -> Vec<(&'static str, &'static [&'static str], &'static str)> {
    registry()
        .iter()
        .flat_map(|m| m.exports.iter().map(move |e| (m.name, m.dlls, e.name)))
        .collect()
}

/// Calls the export `name` of `dll` with up to four integer arguments and
/// returns its result register.
///
/// shortcut: every export is called as `extern "system" fn(usize x4) ->
/// usize`: extra arguments are ignored by the callee, and floating-point or
/// wide arguments and results are not representable. Only exports that need
/// no running title are safe to call.
pub fn call_export(dll: &str, name: &str, args: &[usize]) -> Result<usize, String> {
    if args.len() > 4 {
        return Err("at most 4 integer arguments".into());
    }
    let address = registry()
        .iter()
        .filter(|m| m.serves(dll))
        .flat_map(|m| m.exports.iter())
        .find(|e| e.name == name)
        .map(|e| e.address)
        .ok_or_else(|| format!("no export {name} for {dll}"))?;
    let mut a = [0usize; 4];
    a[..args.len()].copy_from_slice(args);
    // SAFETY: the address is a registered `extern "system"` function; the
    // caller picks exports whose parameters are integers or pointers it can
    // do without, and this passes four register-sized integers.
    let f: extern "system" fn(usize, usize, usize, usize) -> usize =
        unsafe { std::mem::transmute(address) };
    Ok(f(a[0], a[1], a[2], a[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_a_trivial_export() {
        // A retail console has no tools memory: returns 0 (BOOL).
        assert_eq!(call_export("xmem.dll", "XMemIsToolsMemAvailable", &[]), Ok(0));
        assert!(call_export("xmem.dll", "NoSuchExport", &[]).is_err());
        assert!(
            list_exports()
                .iter()
                .any(|e| e.2 == "XMemIsToolsMemAvailable")
        );
    }

    /// `v_add_u32 v0, v1, v2` on WARP.
    #[cfg(windows)]
    #[test]
    fn alu_add_on_warp() {
        // VOP2 v_add_nc_u32 (op 0x25): 0 << 31 | op << 25 | vdst << 17 | vsrc1 << 9 | src0
        let word = 0x25 << 25 | 2 << 9 | (256 + 1);
        assert_eq!(run_alu(&[word], [40, 2, 0, 0], true), Ok(42));
    }
}
