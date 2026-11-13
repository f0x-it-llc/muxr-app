//! The tree's lower half: a 52px row per session AI agent; a tap jumps to its pane. Page-agnostic,
//! vocabulary from `crate::ui::agent_marks`. With no agents [`tree_split`] scrolls the tree
//! alone; with any, tree and agents take half the height each and scroll independently.

use std::rc::Rc;

use frust::authoring::text::{TextAlign, TextOverflow};
use frust::{
    AnyView, CrossAxisAlignment, EdgeInsets, Either, FlexChild, GestureDetector, Padding, SizedBox,
    Theme, View, any, column, container, flexible, inflexible, scroll_view, text,
};

use crate::agents::{AgentStatus, AgentSummary};
use crate::features::terminal::domain::entities::AgentEntry;
use crate::ui::agent_marks::{TILE_SIZE, count_pills, kind_tile, status_dot, status_tone};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::rail::labelled_button;
use super::util::{centered_row, h_gap, sized, stretch_column, v_gap};

pub(crate) const ROW_HEIGHT: f64 = 52.0;
/// No tree level wears this radius, so a row is never mistaken for a pane pill or a tab rect.
pub(crate) const ROW_RADIUS: f64 = 10.0;
/// Fixed, since nothing in `frust` caps a width without also tightening it.
const LOCATION_WIDTH: f64 = 92.0;

const HEADER_SIZE: f32 = 9.0;
const NAME_SIZE: f32 = 11.0;
const STATUS_SIZE: f32 = 9.5;
const LINE_SIZE: f32 = 9.5;
const LOCATION_SIZE: f32 = 8.5;

const HEADER_LABEL: &str = "AI AGENTS";
const LOCATION_SEP: &str = " \u{203a} ";

const ROW_PAD: EdgeInsets = EdgeInsets {
    left: spacing::SM,
    top: spacing::XS,
    right: spacing::MD,
    bottom: spacing::XS,
};
const HEADER_PAD: EdgeInsets = EdgeInsets {
    left: spacing::BASE,
    top: spacing::MD,
    right: spacing::BASE,
    bottom: spacing::SM,
};
const LIST_PAD: EdgeInsets = EdgeInsets {
    left: spacing::BASE,
    top: 0.0,
    right: spacing::BASE,
    bottom: spacing::SM,
};

pub(crate) type PressAgent<State> = Rc<dyn Fn(&mut State, &AgentEntry)>;

/// The pane this connection is on; its agent's row wears the amber border.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FocusedPane {
    pub space_id: Option<String>,
    pub tab_key: String,
    pub pane_id: u32,
}

impl FocusedPane {
    /// An agent with no space id lives in the one space there is.
    fn hosts(&self, agent: &AgentEntry) -> bool {
        agent.pane_id == self.pane_id
            && agent.tab_key == self.tab_key
            && (agent.space_id.is_none() || agent.space_id == self.space_id)
    }
}

pub(crate) struct AgentsData<'a> {
    pub agents: &'a [AgentEntry],
    pub focused: Option<FocusedPane>,
}

pub(crate) struct AgentTones {
    theme: Theme,
    colors: Palette,
}

impl AgentTones {
    pub(crate) fn active() -> Self {
        let theme = active_theme();
        Self {
            colors: Palette::from_theme(&theme),
            theme,
        }
    }
}

pub(crate) fn agents_summary(agents: &[AgentEntry]) -> AgentSummary {
    AgentSummary::from_statuses(
        agents
            .iter()
            .map(|agent| (agent.status, agent.kind.as_str())),
    )
}

pub(crate) fn tree_split<State: 'static, Tree: View<State>, Agents: View<State>>(
    tree: Tree,
    agents: Option<Agents>,
) -> impl View<State> {
    match agents {
        None => Either::Left(scroll_view(tree)),
        Some(agents) => Either::Right(
            column()
                .flex(1, scroll_view(tree))
                .flex(1, agents)
                .cross_axis(CrossAxisAlignment::Stretch),
        ),
    }
}

pub(crate) fn agents_section<State: 'static>(
    data: &AgentsData<'_>,
    on_agent: &PressAgent<State>,
) -> impl View<State> + use<State> {
    let tones = AgentTones::active();
    let mut rows: Vec<AnyView<State>> = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
    for (index, agent) in data.agents.iter().enumerate() {
        if index > 0 {
            rows.push(any(v_gap(spacing::XS)));
        }
        let focused = data.focused.as_ref().is_some_and(|pane| pane.hosts(agent));
        let press = Rc::clone(on_agent);
        let target = agent.clone();
        rows.push(any(agent_row(
            agent,
            focused,
            &tones,
            move |state: &mut State| press(state, &target),
        )));
    }
    column()
        .child(agents_header(&agents_summary(data.agents), &tones))
        .flex(1, scroll_view(Padding(LIST_PAD, stretch_column(rows))))
        .cross_axis(CrossAxisAlignment::Stretch)
}

pub(crate) fn agents_header<State: 'static>(
    summary: &AgentSummary,
    tones: &AgentTones,
) -> impl View<State> {
    let colors = &tones.colors;
    let mut children: Vec<FlexChild<State>> = vec![
        inflexible(
            text(format!("{HEADER_LABEL} \u{b7} {}", summary.total)).style(sized(
                &tones.theme.type_scale.label_small,
                HEADER_SIZE,
                colors.fg_dim,
            )),
        ),
        flexible(1, SizedBox::<State>(None, None)),
    ];
    for (index, pill) in count_pills::<State>(summary).into_iter().enumerate() {
        if index > 0 {
            children.push(h_gap(spacing::TIGHT));
        }
        children.push(inflexible(pill));
    }
    column()
        .child(container(SizedBox::<State>(None, Some(HAIRLINE))).fill(colors.outline))
        .child(Padding(HEADER_PAD, centered_row(children)))
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// Blocked rows wear the amber wash and warning border; the focused pane's row the amber border.
pub(crate) fn agent_row<State: 'static>(
    agent: &AgentEntry,
    focused: bool,
    tones: &AgentTones,
    on_press: impl Fn(&mut State) + 'static,
) -> impl View<State> {
    let (theme, colors) = (&tones.theme, &tones.colors);
    let blocked = agent.status == AgentStatus::Blocked;
    let (fill, border) = if blocked {
        (colors.amber_wash, colors.warning)
    } else if focused {
        (colors.amber_faint, colors.amber_border)
    } else {
        (colors.base, colors.outline)
    };

    let one_line = |value: String, size: f32, ink| {
        text(value)
            .style(sized(&theme.type_scale.label_small, size, ink))
            .max_lines(1)
            .overflow(TextOverflow::Ellipsis)
    };
    let head = centered_row(vec![
        flexible(
            1,
            text(format!("{} \u{b7} {}", agent.display_name(), agent.kind))
                .style(sized(&theme.type_scale.label_large, NAME_SIZE, colors.fg))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
        h_gap(spacing::SM),
        inflexible(text(agent.status.label()).style(sized(
            &theme.type_scale.label_small,
            STATUS_SIZE,
            status_tone(colors, agent.status),
        ))),
    ]);
    let line =
        centered_row(vec![
            flexible(
                1,
                one_line(
                    agent.subtitle().unwrap_or_default().to_string(),
                    LINE_SIZE,
                    colors.fg_muted,
                ),
            ),
            h_gap(spacing::SM),
            inflexible(SizedBox::<State>(Some(LOCATION_WIDTH), None).child(
                one_line(location(agent), LOCATION_SIZE, colors.fg_dim).align(TextAlign::End),
            )),
        ]);
    let content = centered_row(vec![
        inflexible(status_dot::<State>(agent.status)),
        h_gap(spacing::SM),
        inflexible(kind_tile::<State>(&agent.kind, TILE_SIZE)),
        h_gap(spacing::SM),
        flexible(
            1,
            column()
                .child(head)
                .child(v_gap(spacing::MICRO))
                .child(line)
                .cross_axis(CrossAxisAlignment::Stretch),
        ),
    ]);
    let boxed =
        container(SizedBox::<State>(None, Some(ROW_HEIGHT)).child(Padding(ROW_PAD, content)))
            .fill(fill)
            .radius(ROW_RADIUS)
            .border(border, HAIRLINE);
    labelled_button(row_label(agent), GestureDetector(boxed).on_tap(on_press))
}

/// The space is left out when the agent reports none.
pub(crate) fn location(agent: &AgentEntry) -> String {
    let pane = format!("p{}", agent.pane_id);
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    if let Some(space) = agent.space_name.as_deref() {
        parts.push(space);
    }
    parts.push(&agent.tab_name);
    parts.push(&pane);
    parts.join(LOCATION_SEP)
}

/// `"reviewer (claude): needs you, in api › shell › p2"`.
pub(crate) fn row_label(agent: &AgentEntry) -> String {
    format!(
        "{} ({}): {}, in {}",
        agent.display_name(),
        agent.kind,
        agent.status.label(),
        location(agent)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(space: Option<&str>, status: AgentStatus) -> AgentEntry {
        AgentEntry {
            pane_id: 2,
            tab_id: 7,
            tab_key: "shell".to_string(),
            space_id: space.map(str::to_string),
            space_name: space.map(str::to_string),
            tab_name: "shell".to_string(),
            kind: "claude".to_string(),
            name: Some("reviewer".to_string()),
            display_kind: None,
            status,
            title: None,
            summary: None,
            cwd: None,
            focused: false,
            change_seq: 1,
        }
    }

    #[test]
    fn the_location_reads_space_tab_pane_and_drops_an_absent_space() {
        assert_eq!(
            location(&agent(Some("api"), AgentStatus::Idle)),
            "api \u{203a} shell \u{203a} p2"
        );
        assert_eq!(
            location(&agent(None, AgentStatus::Idle)),
            "shell \u{203a} p2"
        );
    }

    #[test]
    fn a_row_announces_its_name_kind_status_and_place_in_one_sentence() {
        assert_eq!(
            row_label(&agent(Some("api"), AgentStatus::Blocked)),
            "reviewer (claude): needs you, in api \u{203a} shell \u{203a} p2"
        );
    }

    #[test]
    fn the_focused_pane_hosts_only_its_own_agent() {
        let pane = FocusedPane {
            space_id: Some("api".to_string()),
            tab_key: "shell".to_string(),
            pane_id: 2,
        };
        assert!(pane.hosts(&agent(Some("api"), AgentStatus::Working)));
        assert!(pane.hosts(&agent(None, AgentStatus::Working)));
        assert!(!pane.hosts(&agent(Some("web"), AgentStatus::Working)));
        let mut elsewhere = agent(Some("api"), AgentStatus::Working);
        elsewhere.pane_id = 3;
        assert!(!pane.hosts(&elsewhere));
    }

    #[test]
    fn the_summary_rolls_up_every_agent() {
        let agents = [
            agent(Some("api"), AgentStatus::Blocked),
            agent(Some("api"), AgentStatus::Done),
        ];
        let summary = agents_summary(&agents);
        assert_eq!((summary.total, summary.blocked, summary.done), (2, 1, 1));
        assert_eq!(summary.kinds, ["claude"]);
    }
}
