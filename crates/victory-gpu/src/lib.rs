//! D3D12.X on host D3D12.
//!
//! D3D12.X records commands from code inlined into the title, which writes
//! packets into the command list while its put pointer is below a limit and
//! calls the list's method otherwise. Our lists keep the limits at zero, so
//! every command arrives here as a call, to be run on host D3D12. The
//! title's GPU addresses are CPU addresses (console memory is unified).
//!
//! [`device`] is the device object titles create, [`dxgi`] its adapter side,
//! [`resource`] heaps and resources, [`fence`] fences, [`queue`] command
//! queues, [`list`] command allocators and lists, [`descriptor`]
//! descriptor heaps, [`root`] root signatures, [`query`] query heaps,
//! [`signature`] command signatures, [`pipeline`] pipeline states,
//! [`frame`] frame pacing, [`object`] what all console graphics objects
//! share, [`console`] which console model the title sees, `external` host
//! libraries recording into title lists. Presentation is in [`queue`] and
//! `host`.

#[cfg(windows)]
mod bindless;
#[cfg(all(windows, feature = "diagnostics"))]
mod capture;
pub mod caps;
pub mod console;
#[cfg(all(windows, feature = "diagnostics"))]
mod copystat;
#[cfg(all(windows, feature = "diagnostics"))]
mod budget;
#[cfg(windows)]
mod detile;
pub mod descriptor;
pub mod device;
#[cfg(windows)]
mod draw;
pub mod dxgi;
#[cfg(windows)]
pub mod external;
pub mod fence;
pub mod frame;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod indirect;
#[cfg(windows)]
mod inplace;
#[cfg(windows)]
pub mod lazy;
#[cfg(windows)]
pub mod ledger;
pub mod list;
#[cfg(all(windows, feature = "diagnostics"))]
pub mod memclass;
#[cfg(windows)]
mod memory;
mod mesh;
pub mod object;
pub mod pipeline;
#[cfg(windows)]
mod primitive;
pub mod query;
pub mod queue;
pub mod resource;
pub mod root;
#[cfg(windows)]
mod sampler;
#[cfg(windows)]
pub mod shader_cache;
#[cfg(windows)]
mod shadow;
pub mod signature;
mod switch;
#[cfg(windows)]
mod tdr;
#[cfg(all(windows, feature = "diagnostics"))]
mod levels;
#[cfg(windows)]
mod texture;
#[cfg(windows)]
mod twin;
#[cfg_attr(not(windows), allow(dead_code))]
mod vram;
#[cfg(all(windows, feature = "diagnostics"))]
mod vramcopy;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
pub use host::on_failure;
#[cfg(windows)]
pub use inplace::title_memory_section;
#[cfg(windows)]
pub use watch::{
    Writing as KernelWrite, memory_kind, title_memory_aliased, title_memory_freed, title_memory_lazy, title_memory_remapped,
    kernel_will_write,
};
