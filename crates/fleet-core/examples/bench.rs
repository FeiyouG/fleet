use std::time::Instant;
fn main() {
    let t = Instant::now();
    let p = fleet_core::procs::ProcTable::snapshot();
    println!(
        "procs: {:?} ({} opencode)",
        t.elapsed(),
        p.named("opencode").len()
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = Instant::now();
    let s = fleet_core::agents::opencode::db::recent_sessions(
        &fleet_core::agents::opencode::db::default_db_path(),
        14 * 86400,
    )
    .unwrap();
    println!("db: {:?} ({} sessions)", t.elapsed(), s.len());
    let cfg = fleet_core::Config::load().unwrap();
    let t = Instant::now();
    let w = rt
        .block_on(fleet_core::worktrees::git::worktrees(&cfg.repos[0]))
        .unwrap();
    println!("git: {:?} ({} wts)", t.elapsed(), w.len());
    let t = Instant::now();
    let known = fleet_core::agents::DiscoverCtx::default();
    let oc = fleet_core::agents::opencode::OpenCode::default();
    use fleet_core::agents::AgentAdapter;
    let a = rt.block_on(oc.discover(&p, &known)).unwrap();
    println!(
        "agents: {:?} ({} rows, {} plans)",
        t.elapsed(),
        a.len(),
        a.iter().filter(|x| x.plan_url.is_some()).count()
    );
    let t = Instant::now();
    let gl =
        fleet_core::reviews::adapter_for(cfg.repos[0].review_source.as_ref().unwrap()).unwrap();
    use fleet_core::reviews::ReviewAdapter;
    let r = rt.block_on(gl.open_reviews()).unwrap();
    println!("reviews: {:?} ({} rows)", t.elapsed(), r.len());
    let eng = fleet_core::Engine::new(cfg.clone());
    let t = Instant::now();
    let f = rt.block_on(eng.snapshot(false));
    println!(
        "snapshot(no-wait): {:?} ({} overview rows, warnings {:?})",
        t.elapsed(),
        f.overview.len(),
        f.warnings
    );
}
