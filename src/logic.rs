//! Pure rules: rank names, the smurf flag, the rank gap, and how numbers are worded.
//! Nothing here does I/O. The reasoning behind the thresholds is in `docs/smurf-score.md`.

use crate::model::Rank;
use crate::store::Summary;

const GROUPS: [&str; 8] = ["Iron", "Bronze", "Silver", "Gold", "Plat", "Dia", "Asc", "Imm"];

/// "Asc 2", "Radiant", "Unranked".
pub fn tier_name(tier: u8) -> String {
    match tier {
        27.. => "Radiant".to_string(),
        3..=26 => format!("{} {}", GROUPS[usize::from((tier - 3) / 3)], (tier - 3) % 3 + 1),
        _ => "Unranked".to_string(),
    }
}

/// Which rank colour a tier uses: 0 none, 1 Iron ... 8 Immortal, 9 Radiant.
pub fn tier_tint(tier: u8) -> u8 {
    match tier {
        27.. => 9,
        3..=26 => (tier - 3) / 3 + 1,
        _ => 0,
    }
}

/// A rank as one number, 100 per division, so ranks can be averaged.
pub fn rating(rank: &Rank) -> Option<i32> {
    (rank.tier >= 3).then(|| i32::from(rank.tier) * 100 + rank.rr.clamp(0, 99))
}

/// The division an averaged rating falls in.
pub fn rating_name(rating: i32) -> String {
    tier_name((rating / 100).clamp(0, 27) as u8)
}

pub fn percent(part: u32, whole: u32) -> u32 {
    if whole == 0 {
        0
    } else {
        (part * 100 + whole / 2) / whole
    }
}

/// Kills per death, to two places: "1.21".
pub fn kd(kills: u32, deaths: u32) -> String {
    let hundredths = (kills * 100 + deaths.max(1) / 2) / deaths.max(1);
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

pub fn adr(damage: u32, rounds: u32) -> u32 {
    if rounds == 0 {
        0
    } else {
        (damage + rounds / 2) / rounds
    }
}

/// "KD 1.21 · HS 26% · ADR 152"
pub fn stat_line(s: &Summary) -> String {
    format!("KD {}  ·  HS {}%  ·  ADR {}", kd(s.kills, s.deaths), percent(s.head, s.shots), adr(s.damage, s.rounds))
}

/// "4d ago", "3h ago", "just now".
pub fn ago(seconds: i64) -> String {
    match seconds {
        ..3_600 => "just now".to_string(),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// Why a player might be on a second account. Each entry is one signal in words.
/// An empty list means "no flag". The flag is deliberately quiet: strong recent results alone
/// never raise it; the account also has to look new.
pub fn smurf_signals(level: Option<u32>, rank: Option<&Rank>, s: &Summary) -> Vec<String> {
    let mut signals = Vec::new();
    let (mut new_account, mut performance) = (0, 0);

    if let Some(level) = level {
        if level < 30 {
            new_account += 2;
            signals.push(format!("level {level}"));
        } else if level < 60 {
            new_account += 1;
            signals.push(format!("level {level}"));
        }
    }
    if let Some(rank) = rank {
        if rank.games > 0 && rank.games < 40 {
            new_account += 1;
            signals.push(format!("{} ranked matches ever", rank.games));
        }
    }
    // Form needs enough matches to mean anything.
    if s.games >= 5 {
        let kd_hundredths = s.kills * 100 / s.deaths.max(1);
        if kd_hundredths >= 160 {
            performance += 2;
            signals.push(format!("KD {}", kd(s.kills, s.deaths)));
        } else if kd_hundredths >= 135 {
            performance += 1;
            signals.push(format!("KD {}", kd(s.kills, s.deaths)));
        }
        let hs = percent(s.head, s.shots);
        if hs >= 33 {
            performance += 2;
            signals.push(format!("HS {hs}%"));
        } else if hs >= 28 {
            performance += 1;
            signals.push(format!("HS {hs}%"));
        }
        if adr(s.damage, s.rounds) >= 185 {
            performance += 1;
            signals.push(format!("ADR {}", adr(s.damage, s.rounds)));
        }
        if s.games >= 8 && percent(s.wins, s.games) >= 70 {
            performance += 1;
            signals.push(format!("won {} of {}", s.wins, s.games));
        }
    }

    if new_account >= 1 && performance >= 2 && new_account + performance >= 4 {
        signals
    } else {
        Vec::new()
    }
}

/// How the two teams compare by rank.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Gap {
    pub ally_avg: i32,
    pub enemy_avg: i32,
    /// My team's estimated chance, 5 to 95.
    pub percent: i32,
}

/// Needs at least three known ranks on each side; otherwise there is nothing honest to say.
pub fn rank_gap(allies: &[i32], enemies: &[i32]) -> Option<Gap> {
    if allies.len() < 3 || enemies.len() < 3 {
        return None;
    }
    let avg = |v: &[i32]| v.iter().sum::<i32>() / v.len() as i32;
    let (ally_avg, enemy_avg) = (avg(allies), avg(enemies));
    // A logistic curve: one division (100) of average difference is about 59%.
    let diff = f64::from(ally_avg - enemy_avg);
    let chance = 1.0 / (1.0 + 10f64.powf(-diff / 600.0));
    Some(Gap { ally_avg, enemy_avg, percent: ((chance * 100.0).round() as i32).clamp(5, 95) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rank(tier: u8, rr: i32, games: u32) -> Rank {
        Rank { tier, rr, peak: tier, games, ..Rank::default() }
    }

    fn form(games: u32, wins: u32, kills: u32, deaths: u32, head: u32, shots: u32, damage: u32) -> Summary {
        Summary { games, wins, rounds: games * 20, kills, deaths, damage, head, shots, ..Summary::default() }
    }

    #[test]
    fn tier_names_and_tints() {
        assert_eq!(tier_name(0), "Unranked");
        assert_eq!(tier_name(3), "Iron 1");
        assert_eq!(tier_name(14), "Gold 3");
        assert_eq!(tier_name(22), "Asc 2");
        assert_eq!(tier_name(26), "Imm 3");
        assert_eq!(tier_name(27), "Radiant");
        assert_eq!((tier_tint(0), tier_tint(3), tier_tint(22), tier_tint(27)), (0, 1, 7, 9));
        assert_eq!(rating(&rank(22, 61, 0)), Some(2261));
        assert_eq!(rating(&rank(0, 0, 0)), None);
        assert_eq!(rating_name(2261), "Asc 2");
    }

    #[test]
    fn number_wording() {
        assert_eq!(kd(23, 19), "1.21");
        assert_eq!(kd(10, 0), "10.00");
        assert_eq!(percent(26, 100), 26);
        assert_eq!(percent(1, 0), 0);
        assert_eq!(adr(3040, 20), 152);
        assert_eq!(ago(120), "just now");
        assert_eq!(ago(3 * 3600 + 5), "3h ago");
        assert_eq!(ago(4 * 86_400 + 99), "4d ago");
    }

    #[test]
    fn a_new_account_with_strong_form_is_flagged() {
        // Level 24, KD 1.8, HS 36%.
        let s = form(8, 7, 180, 100, 36, 100, 8 * 20 * 190);
        let signals = smurf_signals(Some(24), Some(&rank(18, 40, 30)), &s);
        assert!(signals.len() >= 4, "{signals:?}");
        assert!(signals.iter().any(|x| x == "level 24"));
    }

    #[test]
    fn a_veteran_with_strong_form_is_not_flagged() {
        let s = form(10, 8, 180, 100, 36, 100, 10 * 20 * 190);
        assert!(smurf_signals(Some(250), Some(&rank(24, 40, 900)), &s).is_empty());
        // Level hidden and plenty of ranked matches: nothing says the account is new.
        assert!(smurf_signals(None, Some(&rank(24, 40, 900)), &s).is_empty());
    }

    #[test]
    fn a_new_account_with_ordinary_form_is_not_flagged() {
        let s = form(8, 4, 100, 100, 18, 100, 8 * 20 * 130);
        assert!(smurf_signals(Some(22), Some(&rank(9, 40, 12)), &s).is_empty());
        // Too few matches to judge form at all.
        let thin = form(3, 3, 90, 30, 40, 100, 3 * 20 * 220);
        assert!(smurf_signals(Some(22), None, &thin).is_empty());
    }

    #[test]
    fn rank_gap_needs_enough_ranks_and_is_symmetric() {
        assert!(rank_gap(&[2100, 2200], &[2100, 2200, 2300]).is_none());
        let even = rank_gap(&[2100, 2200, 2300], &[2300, 2200, 2100]).unwrap();
        assert_eq!(even.percent, 50);
        let up = rank_gap(&[2200, 2200, 2200], &[2100, 2100, 2100]).unwrap();
        let down = rank_gap(&[2100, 2100, 2100], &[2200, 2200, 2200]).unwrap();
        assert_eq!(up.percent, 59);
        assert_eq!(up.percent + down.percent, 100);
        assert_eq!(rank_gap(&[2700; 5], &[300; 5]).unwrap().percent, 95);
    }
}
