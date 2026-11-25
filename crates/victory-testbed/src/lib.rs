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
/// returns the low 32 bits of its result register (the upper half is
/// undefined for a BOOL or HRESULT).
///
/// # Safety
/// The export is called with whatever the arguments are: it must take only
/// integers, or pointers the arguments make valid, and need no running title.
///
/// shortcut: every export is called as `extern "system" fn(usize x4) ->
/// usize`: extra arguments are ignored by the callee, and floating-point or
/// wide arguments and results are not representable. Only exports that need
/// no running title are safe to call.
pub unsafe fn call_export(dll: &str, name: &str, args: &[usize]) -> Result<u32, String> {
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
    // The upper half of the result register is undefined for 32-bit results.
    Ok(f(a[0], a[1], a[2], a[3]) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_a_trivial_export() {
        // A retail console has no tools memory: returns 0 (BOOL).
        // SAFETY: an export that takes no arguments and touches nothing.
        assert_eq!(unsafe { call_export("xmem.dll", "XMemIsToolsMemAvailable", &[]) }, Ok(0));
        // SAFETY: no such export; the call is refused before any jump.
        assert!(unsafe { call_export("xmem.dll", "NoSuchExport", &[]) }.is_err());
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

/// A decimal or `0x` hexadecimal number.
pub fn parse_num(s: &str) -> Result<u64, String> {
    let s = s.trim();
    match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("{s}: {e}"))
}

/// Up to four comma-separated numbers; missing ones are zero.
pub fn parse_inputs(s: &str) -> Result<[u32; 4], String> {
    let mut out = [0u32; 4];
    for (slot, p) in out.iter_mut().zip(s.split(',')) {
        *slot = u32::try_from(parse_num(p)?).map_err(|e| format!("{p}: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn numbers_and_inputs() {
        assert_eq!(parse_num("0x10"), Ok(16));
        assert_eq!(parse_num("16"), Ok(16));
        assert!(parse_num("zz").is_err());
        assert_eq!(parse_inputs("40,0x2"), Ok([40, 2, 0, 0]));
        assert!(parse_inputs("0x1_0000_0000").is_err());
    }
}
