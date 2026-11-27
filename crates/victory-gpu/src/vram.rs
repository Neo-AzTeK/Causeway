//! Video memory follows the adapter's own budget.
//!
//! The title assumes a console's unified memory; a PC card has 2-24 GB and
//! the OS gives a process part of it (`IDXGIAdapter3::QueryVideoMemoryInfo`,
//! local segment: on an integrated GPU that is the shared memory it may use).
//! [`decide`] is a pure function of (budget, usage): past [`START_PCT`] of the
//! budget, host textures no list reached for a while are evicted, longer-idle
//! first, until usage is back to [`TARGET_PCT`]; the closer to the budget, the
//! shorter the idle time that qualifies. No size is fixed: every figure is a
//! share of the budget. [`resolve`] picks the (budget, usage) pair: the
//! query's, or when it fails the `MemoryPlan` budget (`caps::memory_plan`: the
//! probe's figure, else dedicated or half the shared memory by UMA, else a
//! fixed guess) against the bytes the host made resident. This module asks the
//! adapter for no capability: only the live usage, which `Caps` leaves out.
//!
//! Eviction runs under the submission lock, before the lists run
//! (`resource::make_resident`): the host waits for every submission, so no
//! evicted texture is in flight. Evicted textures are made resident again
//! the next time anything hands them to the GPU (`resource::residency`).

use crate::caps::MemoryPlan;

/// Usage, in percent of the budget, from which idle textures are evicted.
pub(crate) const START_PCT: u64 = 75;
/// Usage, in percent of the budget, eviction frees down to (below
/// [`START_PCT`]: it does not start again at once).
pub(crate) const TARGET_PCT: u64 = 70;
/// Usage from which idle time stops mattering much ([`IDLE_HIGH_MS`]).
pub(crate) const HIGH_PCT: u64 = 85;
/// Usage from which only textures touched this very submission stay.
pub(crate) const CRITICAL_PCT: u64 = 95;
/// Idle time that qualifies a texture below [`HIGH_PCT`].
pub(crate) const IDLE_MS: u64 = 2000;
/// Idle time that qualifies one from [`HIGH_PCT`] on.
pub(crate) const IDLE_HIGH_MS: u64 = 250;
/// Idle time from [`CRITICAL_PCT`] on (a texture a list touched in the last
/// 50 ms stays: it may be in the list about to run).
pub(crate) const IDLE_CRITICAL_MS: u64 = 50;
/// Time between samples of the budget.
pub(crate) const SAMPLE_MS: u64 = 250;
/// Time between log lines of budget and usage.
pub(crate) const LOG_MS: u64 = 1000;

/// What to evict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Bytes to free (0: nothing is evicted).
    pub(crate) free: u64,
    /// Only textures no list reached for this long qualify.
    pub(crate) idle_ms: u64,
}

/// Percent of `budget` that `usage` is (a zero budget is full).
fn percent(budget: u64, usage: u64) -> u64 {
    if budget == 0 {
        return u64::MAX;
    }
    (u128::from(usage) * 100 / u128::from(budget)).min(u128::from(u64::MAX)) as u64
}

/// What to evict at `usage` bytes of a `budget`.
pub(crate) fn decide(budget: u64, usage: u64) -> Decision {
    let pct = percent(budget, usage);
    if pct < START_PCT {
        return Decision { free: 0, idle_ms: IDLE_MS };
    }
    let target = (u128::from(budget) * u128::from(TARGET_PCT) / 100) as u64;
    let idle_ms = match pct {
        p if p >= CRITICAL_PCT => IDLE_CRITICAL_MS,
        p if p >= HIGH_PCT => IDLE_HIGH_MS,
        _ => IDLE_MS,
    };
    Decision { free: usage.saturating_sub(target), idle_ms }
}

/// The (budget, usage) to decide on: the query's `(budget, current usage)`,
/// else (the plan's budget, bytes `own` the host made resident). A query that
/// reports no budget counts as failed.
pub(crate) fn resolve(query: Option<(u64, u64)>, plan: MemoryPlan, own: u64) -> (u64, u64) {
    match query {
        Some((budget, usage)) if budget > 0 => (budget, usage),
        _ => (plan.budget, own),
    }
}

/// Whether host textures may be made ahead of any list's use (the texture
/// queue of `descriptor::host`): usage under [`START_PCT`] of the budget, so
/// that making them never starts eviction (which would take back what was
/// just made, and textures shaders read by index are not stamped by lists).
pub(crate) fn has_room(budget: u64, usage: u64) -> bool {
    percent(budget, usage) < START_PCT
}

/// Textures to evict: of `(key, idle_ms, bytes)`, those idle for at least
/// `decision.idle_ms`, longest idle first, until `decision.free` bytes are
/// freed (the one that crosses the line included). Keys, in that order.
pub(crate) fn pick<K: Copy>(decision: Decision, candidates: &[(K, u64, u64)]) -> Vec<K> {
    if decision.free == 0 {
        return Vec::new();
    }
    let mut idle: Vec<_> = candidates.iter().filter(|c| c.1 >= decision.idle_ms).collect();
    idle.sort_by_key(|c| std::cmp::Reverse(c.1));
    let mut freed = 0u64;
    let mut out = Vec::new();
    for c in idle {
        if freed >= decision.free {
            break;
        }
        freed = freed.saturating_add(c.2);
        out.push(c.0);
    }
    out
}

#[cfg(windows)]
pub(crate) use host::{init, room, tick};

#[cfg(windows)]
mod host {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    use windows::Win32::Graphics::Dxgi::{
        DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3,
    };

    struct Monitor {
        adapter: Option<IDXGIAdapter3>,
        plan: MemoryPlan,
        state: Mutex<State>,
    }

    struct State {
        sampled: Option<Instant>,
        logged: Option<Instant>,
    }

    static MONITOR: OnceLock<Monitor> = OnceLock::new();

    /// The host's adapter for live usage (`None`: the query is not
    /// available) and the caps the budget plan comes from.
    pub(crate) fn init(adapter: Option<IDXGIAdapter3>, caps: &crate::caps::Caps) {
        let plan = crate::caps::memory_plan(caps);
        tracing::info!("video memory plan: {} MB ({:?})", plan.budget >> 20, plan.source);
        let _ = MONITOR.set(Monitor { adapter, plan, state: Mutex::new(State { sampled: None, logged: None }) });
    }

    /// Local-segment (budget, usage) of the adapter, if it answers.
    fn query(adapter: &IDXGIAdapter3) -> Option<(u64, u64)> {
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: a live adapter and an out structure.
        unsafe { adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }
            .ok()
            .map(|()| (info.Budget, info.CurrentUsage))
    }

    /// [`has_room`] now, from the adapter's live figures (else the plan's
    /// budget against the host textures resident). Before [`init`] (tests
    /// without a device): room.
    pub(crate) fn room() -> bool {
        let Some(monitor) = MONITOR.get() else {
            return true;
        };
        let asked = monitor.adapter.as_ref().and_then(query);
        let (budget, usage) = resolve(asked, monitor.plan, crate::resource::resident_bytes());
        has_room(budget, usage)
    }

    /// At most every [`SAMPLE_MS`]: samples budget and usage, logs them at
    /// most every [`LOG_MS`], and returns what to evict given `own` bytes
    /// of host textures resident. `None` between samples.
    pub(crate) fn tick(own: u64) -> Option<Decision> {
        let monitor = MONITOR.get()?;
        let now = Instant::now();
        let mut state = monitor.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.sampled.is_some_and(|t| now.duration_since(t).as_millis() < u128::from(SAMPLE_MS)) {
            return None;
        }
        state.sampled = Some(now);
        let asked = monitor.adapter.as_ref().and_then(query);
        let (budget, usage) = resolve(asked, monitor.plan, own);
        let decision = decide(budget, usage);
        if state.logged.is_none_or(|t| now.duration_since(t).as_millis() >= u128::from(LOG_MS)) {
            state.logged = Some(now);
            // System memory the driver holds for our GPU objects (upload
            // heaps, what it moved out of video memory): part of the
            // process's private memory that no owner of `crate::ledger` names.
            let system = monitor.adapter.as_ref().map_or(0, |a| {
                let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
                // SAFETY: a live adapter and an out structure.
                unsafe { a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut info) }
                    .map_or(0, |()| info.CurrentUsage)
            });
            tracing::info!(
                "video memory: {:.0} MB used of a {:.0} MB budget ({}), host textures resident {:.0} MB, evicting {:.0} MB; system memory the GPU uses {:.0} MB",
                usage as f64 / 1e6,
                budget as f64 / 1e6,
                if asked.is_some() { "DXGI" } else { "query failed: caps plan" },
                own as f64 / 1e6,
                decision.free as f64 / 1e6,
                system as f64 / 1e6,
            );
        }
        Some(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    /// Fake adapters: (dedicated, shared), budget the OS gives (about 90%
    /// of dedicated; half the shared memory for an integrated GPU).
    /// (Multiples of 100 bytes: shares in percent are exact.)
    const MATRIX: [(&str, u64); 5] = [
        ("integrated", GB / 100 * 100),
        ("2 GB", 2 * GB * 9 / 10 / 100 * 100),
        ("4 GB", 4 * GB * 9 / 10 / 100 * 100),
        ("8 GB", 8 * GB * 9 / 10 / 100 * 100),
        ("24 GB", 24 * GB * 9 / 10 / 100 * 100),
    ];

    #[test]
    fn nothing_is_evicted_below_the_start_line() {
        for (name, budget) in MATRIX {
            let usage = budget * (START_PCT - 1) / 100;
            assert_eq!(decide(budget, usage).free, 0, "{name}");
            assert_eq!(decide(budget, 0).free, 0, "{name}");
        }
    }

    #[test]
    fn eviction_frees_down_to_the_target_share_of_each_budget() {
        for (name, budget) in MATRIX {
            for pct in [START_PCT, 80, HIGH_PCT, 90, CRITICAL_PCT, 100, 120] {
                let usage = budget * pct / 100;
                let d = decide(budget, usage);
                // What is left is the target share, whatever the card.
                let left = usage - d.free;
                assert_eq!(left, budget * TARGET_PCT / 100, "{name} at {pct}%");
            }
        }
    }

    #[test]
    fn idle_time_shrinks_as_usage_nears_the_budget() {
        for (name, budget) in MATRIX {
            let idle = |pct: u64| decide(budget, budget * pct / 100).idle_ms;
            assert_eq!(idle(START_PCT), IDLE_MS, "{name}");
            assert_eq!(idle(HIGH_PCT), IDLE_HIGH_MS, "{name}");
            assert_eq!(idle(CRITICAL_PCT), IDLE_CRITICAL_MS, "{name}");
            assert!(idle(START_PCT) > idle(HIGH_PCT) && idle(HIGH_PCT) > idle(CRITICAL_PCT), "{name}");
        }
    }

    #[test]
    fn the_policy_scales_with_the_budget_not_with_a_size() {
        // The same share of any budget gives the same share to free.
        let share = |budget: u64| decide(budget, budget * 90 / 100).free as f64 / budget as f64;
        for (_, budget) in MATRIX {
            assert!((share(budget) - 0.20).abs() < 1e-6);
        }
        // The same absolute usage is pressure on a small card only.
        assert_eq!(decide(24 * GB * 9 / 10, 2 * GB).free, 0);
        assert!(decide(2 * GB * 9 / 10, 2 * GB).free > 0);
    }

    #[test]
    fn a_zero_budget_evicts_what_it_can_without_overflow() {
        let d = decide(0, 100);
        assert_eq!(d.free, 100);
        assert_eq!(d.idle_ms, IDLE_CRITICAL_MS);
        assert_eq!(decide(u64::MAX, u64::MAX).idle_ms, IDLE_CRITICAL_MS);
        assert_eq!(decide(0, 0).free, 0);
    }

    #[test]
    fn a_failed_query_falls_back_to_the_plan_then_to_a_default() {
        use crate::caps::{BudgetSource, Caps, FALLBACK_BUDGET, MemoryCaps, memory_plan};
        let caps = |uma, budget, dedicated, shared| Caps {
            uma,
            memory: MemoryCaps { budget, dedicated, shared },
            ..Caps::unknown()
        };
        let own = 3 * GB;
        // Fake adapters: small VRAM, UMA with a tiny dedicated window,
        // large VRAM with a probe-time budget, discrete (the 2060 SUPER
        // class) with no budget, nothing known.
        let small = memory_plan(&caps(Some(false), None, 2 * GB, 16 * GB));
        let uma = memory_plan(&caps(Some(true), None, GB / 8, 16 * GB));
        let large = memory_plan(&caps(Some(false), Some(21 * GB), 24 * GB, 64 * GB));
        let discrete = memory_plan(&caps(Some(false), None, 8 * GB, 16 * GB));
        let nothing = memory_plan(&Caps::unknown());
        assert_eq!([small.budget, uma.budget, large.budget], [2 * GB, 8 * GB, 21 * GB]);
        // As the old adapter fallback: dedicated size on a discrete card.
        assert_eq!(discrete.budget, 8 * GB);
        assert_eq!((nothing.budget, nothing.source), (FALLBACK_BUDGET, BudgetSource::Fallback));
        for plan in [small, uma, large, discrete, nothing] {
            // The live query wins.
            assert_eq!(resolve(Some((7 * GB, 5 * GB)), plan, own), (7 * GB, 5 * GB));
            // Failed or empty: the plan's budget, the host's own bytes.
            assert_eq!(resolve(None, plan, own), (plan.budget, own));
            assert_eq!(resolve(Some((0, 5 * GB)), plan, own), (plan.budget, own));
        }
        // The policy still works on the fallback: 3 GB of 4 GB is pressure.
        let (budget, usage) = resolve(None, nothing, own);
        assert!(decide(budget, usage).free > 0);
        assert_eq!(decide(budget, 2 * GB).free, 0);
        // A small card is under pressure at the same own bytes, a large one not.
        assert!(decide(small.budget, own).free > 0);
        assert_eq!(decide(large.budget, own).free, 0);
    }

    #[test]
    fn the_longest_idle_go_first_until_enough_is_freed() {
        let d = Decision { free: 250, idle_ms: 100 };
        // (key, idle ms, bytes)
        let c = [(1, 500, 100), (2, 50, 1000), (3, 900, 100), (4, 300, 100), (5, 200, 100)];
        // 2 has not been idle long enough; 3, 1 and 4 free 300 >= 250.
        assert_eq!(pick(d, &c), [3, 1, 4]);
        assert!(pick(Decision { free: 0, idle_ms: 0 }, &c).is_empty());
        // Not enough idle ones: all of them go, no more.
        assert_eq!(pick(Decision { free: 10_000, idle_ms: 400 }, &c), [3, 1]);
    }
}
