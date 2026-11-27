use my_warp::acp::{AcpSessionState, AcpTurn, Agent};
use my_warp::ui::{
    acp_caret_at, acp_selected_text, discover_system_mono_fonts, FontRenderer, Layout,
};

#[test]
fn transcript_text_can_be_selected_by_pointer() {
    let fonts = discover_system_mono_fonts();
    let mut font = FontRenderer::load_with_config(None, 15.0, &fonts).unwrap();
    let layout = Layout::compute_with_sizes(1200, 800, true, 240, true, None);
    let mut state = AcpSessionState::default();
    state.open = true;
    state.turns.push(AcpTurn {
        seq: 1,
        agent: Agent::Codex,
        prompt: "hello".into(),
        response: "```sh\ncargo test\n```".into(),
        truncated: false,
    });
    let area = layout.acp_messages_rect().unwrap();
    let right = area.x + area.w - 1;

    let start = acp_caret_at(&layout, &state, &mut font, area.x, area.y + 2, false).unwrap();
    let end = acp_caret_at(&layout, &state, &mut font, right, area.y + 20, false).unwrap();
    assert_eq!((start, end), ((0, 0), (1, 5)));
    assert_eq!(acp_selected_text(&layout, &state, &mut font, start, end), "> hello\nCodex");
    assert_eq!(acp_selected_text(&layout, &state, &mut font, end, start), "> hello\nCodex");
    assert_eq!(acp_selected_text(&layout, &state, &mut font, (1, 0), (1, 2)), "Co");
    assert_eq!(acp_selected_text(&layout, &state, &mut font, (3, 0), (3, 10)), "cargo test");
    assert_eq!(acp_selected_text(&layout, &state, &mut font, (0, 3), (0, 3)), "");

    assert!(acp_caret_at(&layout, &state, &mut font, 0, 0, false).is_none());
    assert_eq!(acp_caret_at(&layout, &state, &mut font, 0, 0, true), Some((0, 0)));
    let below = acp_caret_at(&layout, &state, &mut font, right, area.y + area.h + 50, true).unwrap();
    assert!(below.0 >= 3, "{below:?}");

    state.turns.clear();
    state.open = false;
    let closed = Layout::compute_with_sizes(1200, 800, true, 240, false, None);
    assert!(acp_caret_at(&closed, &state, &mut font, right, area.y + 2, true).is_none());
}
