use gpui::{AppContext, TestAppContext};

struct Counter(u32);

#[gpui::test]
fn test_app_context_runs_entities_and_tasks(cx: &mut TestAppContext) {
    let c = cx.new(|_| Counter(0));
    c.update(cx, |c, _| c.0 += 1);
    let exec = cx.executor();
    let task = cx.background_executor.spawn(async move { 41 + 1 });
    let n = exec.block(task);
    assert_eq!((c.read_with(cx, |c, _| c.0), n), (1, 42));
}
