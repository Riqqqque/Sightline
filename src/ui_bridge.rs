//! Turns a `View` from the worker into Slint properties. Runs on the UI thread.

use crate::model::{Phase, Row, Side, View};
use crate::{AppWindow, MapLine, PlayerRow, UiPhase};
use slint::{Model, ModelRc, SharedString, VecModel};

/// The separator the worker puts between the parts of a line.
const DOT: &str = "  ·  ";

/// "KD 1.21  ·  HS 26%  ·  ADR 152" as its three values. Empty strings while there are none.
fn numbers(stats: &str) -> (String, String, String) {
    let part = |label: &str| {
        stats.split(DOT).find_map(|p| p.trim().strip_prefix(label)).map(|v| v.trim().to_string()).unwrap_or_default()
    };
    (part("KD"), part("HS"), part("ADR"))
}

/// A value at or above this stands out: it is drawn in the warning colour.
fn hot(value: &str, at: f32) -> bool {
    value.trim_end_matches('%').parse::<f32>().is_ok_and(|v| v >= at)
}

fn rows(list: Vec<Row>, prefix: char, selectable: bool) -> Vec<PlayerRow> {
    list.into_iter()
        .enumerate()
        .map(|(index, r)| {
            let card = r.card;
            let (kd, hs, adr) = numbers(&card.stats);
            PlayerRow {
                // A hidden row has no key: it cannot be selected, so no note can be attached.
                // Nor has a scoreboard row: its place is not a place in the lobby.
                key: if r.hidden || !selectable { SharedString::new() } else { format!("{prefix}{index}").into() },
                caps: r.name.to_uppercase().into(),
                name: SharedString::from(r.name),
                tag: SharedString::from(r.tag),
                hidden: r.hidden,
                me: r.is_me,
                unnamed: r.unnamed,
                initial: match r.agent.chars().next() {
                    Some(first) => first.to_uppercase().collect::<String>().into(),
                    None if r.picked => "?".into(),
                    None => SharedString::new(),
                },
                role: SharedString::from(r.role),
                picked: r.picked,
                agent: SharedString::from(r.agent),
                locked: r.locked,
                level: r.level.map_or(-1, |l| l.min(i32::MAX as u32) as i32),
                party: i32::from(r.party),
                stats_on: card.stats_on,
                rank_done: card.rank_done,
                rank_caps: card.rank.to_uppercase().into(),
                rank: SharedString::from(card.rank),
                tier: i32::from(card.tier),
                rr: SharedString::from(card.rr),
                peak: SharedString::from(card.peak),
                stats_done: card.stats_done,
                form: ModelRc::new(VecModel::from(card.dots.iter().map(|d| i32::from(*d)).collect::<Vec<_>>())),
                kd_hot: hot(&kd, 1.8),
                hs_hot: hot(&hs, 35.0),
                adr_hot: hot(&adr, 195.0),
                kd: kd.into(),
                hs: hs.into(),
                adr: adr.into(),
                seen_short: card.seen.split(DOT).next().unwrap_or("").into(),
                seen: SharedString::from(card.seen),
                smurf: card.smurf,
                signals: card.signals.replace(DOT, "  //  ").into(),
                mains: SharedString::from(card.mains),
                on_map: SharedString::from(card.on_map),
                note: SharedString::from(card.note),
            }
        })
        .collect()
}

/// A row's key back to its place in the roster: "a2" is the third player on my team.
pub fn place(key: &str) -> Option<(Side, usize)> {
    let side = match key.chars().next()? {
        'a' => Side::Ally,
        'e' => Side::Enemy,
        _ => return None,
    };
    Some((side, key.get(1..)?.parse().ok()?))
}

/// Who the side panel shows when nothing is clicked: the opponent with the best recent KD, or
/// failing that the first one that can be shown. In a match without teams everyone else is an
/// opponent. While only my own team is known (agent select), it shows me.
fn spotlight<'a>(allies: &'a [PlayerRow], enemies: &'a [PlayerRow], one_list: bool) -> Option<&'a PlayerRow> {
    let pool: &[PlayerRow] = if one_list { allies } else { enemies };
    let shown = || pool.iter().filter(|r| !r.hidden && !r.me);
    shown()
        .filter(|r| !r.kd.is_empty())
        .max_by(|a, b| a.kd.parse::<f32>().unwrap_or(0.0).total_cmp(&b.kd.parse::<f32>().unwrap_or(0.0)))
        .or_else(|| shown().next())
        .or_else(|| allies.iter().find(|r| r.me))
}

pub fn apply(ui: &AppWindow, view: View) {
    ui.set_phase(match view.phase {
        Phase::Closed => UiPhase::Closed,
        Phase::Menu => UiPhase::Menu,
        Phase::Queue => UiPhase::Queue,
        Phase::AgentSelect => UiPhase::AgentSelect,
        Phase::InGame => UiPhase::InGame,
        Phase::PostGame => UiPhase::PostGame,
    });
    ui.set_status(view.status.to_uppercase().into());
    ui.set_detail(SharedString::from(view.detail));
    ui.set_map(view.map.to_uppercase().into());
    ui.set_map_key(SharedString::from(view.map));
    ui.set_queue(view.queue.to_uppercase().into());
    ui.set_one_list(view.one_list);

    ui.set_score(view.score.into());
    ui.set_board(view.board);
    let allies = rows(view.allies, 'a', !view.board);
    let enemies = rows(view.enemies, 'e', !view.board);
    // The side panel follows the clicked player while they are in the lobby; otherwise it shows
    // the one picked for it. The note being typed is only replaced when the player changes.
    let clicked = ui.get_sel_key();
    let found = allies.iter().chain(enemies.iter()).find(|r| !clicked.is_empty() && r.key == clicked);
    if found.is_none() && !clicked.is_empty() {
        ui.set_sel_key(SharedString::new());
    }
    // A note is being typed against the player the panel picked by itself: keep that player
    // there while they are still in the lobby, even if someone else now has the better numbers.
    let shown = ui.get_sel_row();
    let typing = clicked.is_empty() && !shown.key.is_empty() && ui.get_note_draft() != shown.note;
    let kept = if typing { allies.iter().chain(enemies.iter()).find(|r| r.key == shown.key) } else { None };
    let pick = if view.board { None } else { spotlight(&allies, &enemies, view.one_list) };
    match found.or(kept).or(pick) {
        Some(row) => {
            if ui.get_sel_row().key != row.key {
                ui.set_note_draft(row.note.clone());
                ui.set_note_saved(false);
            }
            ui.set_sel_row(row.clone());
        }
        None => ui.set_sel_row(PlayerRow::default()),
    }
    ui.set_allies(ModelRc::new(VecModel::from(allies)));
    ui.set_enemies(ModelRc::new(VecModel::from(enemies)));

    let p = view.panel;
    ui.set_my_rank(p.my_rank.to_uppercase().into());
    ui.set_session_title(p.session_title.into());
    ui.set_session_line(p.session_line.into());
    ui.set_session_extra(p.session_extra.into());
    let dots: Vec<i32> = p.session_dots.iter().map(|d| i32::from(*d)).collect();
    if ui.get_session_dots().iter().ne(dots.iter().copied()) {
        ui.set_session_dots(ModelRc::new(VecModel::from(dots)));
    }
    let maps: Vec<MapLine> =
        p.maps.into_iter().map(|m| MapLine { name: m.name.to_uppercase().into(), pct: i32::from(m.pct), line: m.line.into() }).collect();
    if ui.get_maps().iter().ne(maps.iter().cloned()) {
        ui.set_maps(ModelRc::new(VecModel::from(maps)));
    }
    ui.set_recap_result(i32::from(p.recap_result));
    ui.set_recap_title(p.recap_title.to_uppercase().into());
    ui.set_recap_line(p.recap_line.into());
    ui.set_recap_extra(p.recap_extra.into());
    ui.set_nudge(p.nudge.into());
    ui.set_map_line(p.map_line.into());
    ui.set_map_win(p.map_win.into());
    ui.set_gap_pct(p.gap_pct);
    ui.set_gap_label(p.gap_label.into());
    ui.set_stats_status(p.stats_status.into());
    ui.set_store_status(p.store_status.into());
    ui.set_update(p.update.into());
    ui.set_update_status(p.update_status.into());
    for (key, value) in p.settings {
        let value = SharedString::from(value);
        match key.as_str() {
            "lookup" => ui.set_set_lookup(value),
            "smurf" => ui.set_set_smurf(value),
            "estimate" => ui.set_set_estimate(value),
            "seen" => ui.set_set_seen(value),
            "nudge" => ui.set_set_nudge(value),
            "after" => ui.set_set_after(value),
            "updates" => ui.set_set_updates(value),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, kd: &str) -> PlayerRow {
        PlayerRow { key: key.into(), kd: kd.into(), ..PlayerRow::default() }
    }

    #[test]
    fn the_stat_line_splits_into_its_three_numbers() {
        assert_eq!(numbers("KD 1.21  ·  HS 26%  ·  ADR 152"), ("1.21".into(), "26%".into(), "152".into()));
        assert_eq!(numbers(""), (String::new(), String::new(), String::new()));
        assert!(hot("2.06", 1.8) && !hot("1.44", 1.8) && hot("38%", 35.0) && !hot("", 1.0));
    }

    #[test]
    fn the_side_panel_picks_the_strongest_visible_opponent() {
        let me = PlayerRow { me: true, ..row("a0", "1.20") };
        let allies = vec![me.clone(), row("a1", "0.96")];
        let hidden = PlayerRow { hidden: true, ..PlayerRow::default() };
        let enemies = vec![row("e0", "1.44"), row("e1", "2.06"), hidden.clone()];
        assert_eq!(spotlight(&allies, &enemies, false).unwrap().key, "e1");
        // Before any stats have arrived: the first opponent that can be shown.
        assert_eq!(spotlight(&allies, &[hidden.clone(), row("e1", "")], false).unwrap().key, "e1");
        // A match without teams: the best of everyone else.
        assert_eq!(spotlight(&allies, &[], true).unwrap().key, "a1");
        // Agent select, only my team known: me, not a teammate labelled as a threat.
        assert_eq!(spotlight(&allies, &[], false).unwrap().key, "a0");
        // A hidden player is never put in the panel.
        assert_eq!(spotlight(&[me.clone()], &[hidden], false).unwrap().key, "a0");
        assert!(spotlight(&[], &[], false).is_none());
    }

    #[test]
    fn row_keys_map_back_to_places() {
        assert_eq!(place("a0"), Some((Side::Ally, 0)));
        assert_eq!(place("e4"), Some((Side::Enemy, 4)));
        assert_eq!(place(""), None);
        assert_eq!(place("x1"), None);
        assert_eq!(place("a"), None);
    }
}
