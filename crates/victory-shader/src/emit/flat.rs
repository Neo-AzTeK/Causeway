//! A substitute pixel shader for a console pixel program the host cannot
//! run: it takes no inputs and writes one flat colour (magenta, so a
//! substituted draw shows) to each of the render targets named. The draw
//! then still covers the pixels the geometry covers, with the title's
//! blend, depth and raster state; only the shading is wrong.

use super::bitcode::{Attr, Term};
use super::container::SigElement;
use super::shader::Shader;

/// The colour every substituted target gets.
pub const FLAT_COLOUR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];

/// The container of a pixel shader writing [`FLAT_COLOUR`] to each render
/// target slot in `targets` (floating-point, unorm or snorm formats; a
/// slot of an integer format is left out by the caller). No slots: a
/// shader with no outputs, for depth-only passes.
pub fn flat_pixel_shader(targets: &[u32]) -> Vec<u8> {
    let mut s = Shader::new(true);
    let b = s.entry;
    let consts = FLAT_COLOUR.map(|c| s.m.f32(c.to_bits()));
    let mut outputs = Vec::new();
    for (id, &slot) in targets.iter().enumerate() {
        outputs.push(SigElement {
            name: "SV_Target".into(),
            index: slot,
            kind: 16,
            comp: 9,
            interp: 0,
            row: Some(slot),
            col: 0,
            cols: 4,
            usage: 0xf,
            half: false,
        });
        for (col, &v) in consts.iter().enumerate() {
            let args = [s.m.i32(id as u32), s.m.i32(0), s.m.i8(col as u8), v];
            s.op(b, "storeOutput.f32", Attr::NoUnwind, s.t.void, 5, &args);
        }
    }
    s.outputs = outputs;
    s.m.terminate(b, Term::Ret);
    s.finish()
}

#[cfg(all(test, windows))]
mod tests {
    use super::super::bitcode::{Attr, Term};
    use super::super::host::{Draw, Host};
    use super::super::shader::Shader;
    use super::*;

    /// A vertex shader passing POSITION through.
    fn vertex() -> Vec<u8> {
        let mut s = Shader::new(false);
        let b = s.entry;
        let element = |name: &str, kind: u8| SigElement {
            name: name.into(),
            index: 0,
            kind,
            comp: 9,
            interp: 0,
            row: Some(0),
            col: 0,
            cols: 4,
            usage: 0xf,
            half: false,
        };
        for col in 0..4u8 {
            let args = [s.m.i32(0), s.m.i32(0), s.m.i8(col), s.m.undef(s.t.i32)];
            let v = s.op(b, "loadInput.f32", Attr::ReadNone, s.t.f32, 4, &args);
            let args = [s.m.i32(0), s.m.i32(0), s.m.i8(col), v];
            s.op(b, "storeOutput.f32", Attr::NoUnwind, s.t.void, 5, &args);
        }
        s.inputs = vec![element("POSITION", 0)];
        s.outputs = vec![element("SV_Position", 3)];
        s.m.terminate(b, Term::Ret);
        s.finish()
    }

    /// The shader validates, and a draw with it covers the target with
    /// the flat colour (on WARP and on the hardware adapter).
    #[test]
    fn the_flat_shader_draws_its_colour() {
        let vs = vertex();
        for targets in [&[][..], &[0]] {
            let mut ps = flat_pixel_shader(targets);
            if let Ok(mut v) = super::super::validator::Validator::load(
                super::super::validator::Validator::default_path().as_deref(),
            ) {
                v.validate(&mut ps).unwrap();
            }
        }
        let ps = flat_pixel_shader(&[0]);
        let vertices = [-1.0, -1.0, 0.5, 1.0, -1.0, 3.0, 0.5, 1.0, 3.0, -1.0, 0.5, 1.0];
        for warp in [false, true] {
            let Ok(host) = Host::new(warp) else { continue };
            let root = host.root_signature_for(&[]).unwrap();
            let pso = host.pipeline_with(&root, &vs, &ps).unwrap();
            let draw = Draw {
                roots: Vec::new(),
                textures: &[],
                samplers: &[],
                vertices: &vertices,
                stride: 16,
                size: (8, 8),
                mesh_groups: 0,
            };
            let texels = host.draw(&root, &pso, &draw).unwrap();
            assert!(texels.iter().all(|t| *t == FLAT_COLOUR), "warp {warp}: {texels:?}");
        }
    }
}
