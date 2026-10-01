//! Reading per-Pokémon stats out of replay logs.

use pokemon_draft_site::stats::{GameMon, read};

fn log(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("json");
    v["log"].as_str().expect("log").to_owned()
}

fn mon<'a>(mons: &'a [GameMon], species: &str) -> &'a GameMon {
    mons.iter().find(|m| m.species == species).unwrap_or_else(|| panic!("no {species} in {mons:#?}"))
}

/// (played, led, fainted, direct KOs, passive KOs)
fn line(m: &GameMon) -> (bool, bool, bool, i64, i64) {
    (m.played, m.led, m.fainted, m.direct_kos, m.passive_kos)
}

#[test]
fn reads_the_real_replay() {
    let mons = read(&log(include_str!("replays/regmc-sweep.json")));
    assert_eq!(mons.len(), 12);
    let expect = [
        ("Greninja-Mega", (true, true, false, 1, 0)),
        ("Bellibolt", (true, false, false, 2, 0)),
        ("Sableye", (true, true, false, 0, 0)),
        ("Diggersby", (true, false, false, 1, 0)),
        ("Vanilluxe", (false, false, false, 0, 0)),
        ("Mawile", (false, false, false, 0, 0)),
        ("Basculegion", (true, true, true, 0, 0)),
        ("Incineroar", (true, false, true, 0, 0)),
        ("Metagross-Mega", (true, true, true, 0, 0)),
        ("Dragonite", (true, false, true, 0, 0)),
        ("Salazzle", (false, false, false, 0, 0)),
        ("Forretress", (false, false, false, 0, 0)),
    ];
    for (species, want) in expect {
        assert_eq!(line(mon(&mons, species)), want, "{species}");
    }

    let greninja = mon(&mons, "Greninja-Mega");
    assert_eq!(greninja.previewed, "Greninja");
    assert!(greninja.mega);
    assert_eq!(greninja.moves.iter().map(String::as_str).collect::<Vec<_>>(), ["Dark Pulse", "Flip Turn", "Protect"]);
    assert!(greninja.items.contains("Greninjite"));
    assert!(greninja.abilities.contains("Protean"));

    let incineroar = mon(&mons, "Incineroar");
    assert!(incineroar.items.contains("Sitrus Berry"));
    assert!(incineroar.abilities.contains("Intimidate"));
    assert!(mon(&mons, "Bellibolt").abilities.contains("Electromorphosis"));
    assert!(mon(&mons, "Metagross-Mega").items.contains("Metagrossite"));
}

/// Two per side, all four on the field, then `body`.
fn game(body: &str) -> Vec<GameMon> {
    read(&format!(
        "|poke|p1|Sableye, L50|\n|poke|p1|Garchomp, L50|\n|poke|p2|Metagross, L50|\n|poke|p2|Dragonite, L50|\n\
         |switch|p1a: Sableye|Sableye, L50|100/100\n|switch|p1b: Garchomp|Garchomp, L50|100/100\n\
         |switch|p2a: Metagross|Metagross, L50|100/100\n|switch|p2b: Dragonite|Dragonite, L50|100/100\n|turn|1\n{body}"
    ))
}

#[test]
fn a_spread_move_credits_each_ko() {
    let mons = game(
        "|move|p1b: Garchomp|Rock Slide|p2a: Metagross|[spread] p2a,p2b\n\
         |-damage|p2a: Metagross|0 fnt\n|-damage|p2b: Dragonite|0 fnt\n|faint|p2a: Metagross\n|faint|p2b: Dragonite\n",
    );
    assert_eq!(mon(&mons, "Garchomp").direct_kos, 2);
}

#[test]
fn a_burn_ko_credits_whoever_burned() {
    let mons = game(
        "|move|p1a: Sableye|Will-O-Wisp|p2a: Metagross\n|-status|p2a: Metagross|brn\n|\n|upkeep\n|turn|2\n\
         |move|p1b: Garchomp|Protect|p1b: Garchomp\n|\n|-damage|p2a: Metagross|0 fnt|[from] brn\n|faint|p2a: Metagross\n",
    );
    assert_eq!(line(mon(&mons, "Sableye")), (true, true, false, 0, 1));
    assert_eq!(mon(&mons, "Garchomp").direct_kos, 0);
}

#[test]
fn a_partner_ko_credits_no_one() {
    let mons = game(
        "|move|p1b: Garchomp|Earthquake|p1a: Sableye|[spread] p1a,p2a,p2b\n\
         |-damage|p1a: Sableye|0 fnt\n|-damage|p2a: Metagross|50/100\n|-damage|p2b: Dragonite|50/100\n|faint|p1a: Sableye\n",
    );
    assert!(mons.iter().all(|m| m.direct_kos + m.passive_kos == 0), "{mons:#?}");
    assert!(mon(&mons, "Sableye").fainted);
}

#[test]
fn an_of_line_credits_the_of_pokemon() {
    let mons = game(
        "|move|p2a: Metagross|Meteor Mash|p1b: Garchomp\n|-damage|p1b: Garchomp|60/100\n\
         |-damage|p2a: Metagross|0 fnt|[from] ability: Rough Skin|[of] p1b: Garchomp\n|faint|p2a: Metagross\n",
    );
    let chomp = mon(&mons, "Garchomp");
    assert_eq!((chomp.direct_kos, chomp.passive_kos), (0, 1));
    assert!(chomp.abilities.contains("Rough Skin"));
}

#[test]
fn recoil_credits_no_one() {
    let mons = game(
        "|move|p2b: Dragonite|Double-Edge|p1a: Sableye\n|-damage|p1a: Sableye|40/100\n\
         |-damage|p2b: Dragonite|0 fnt|[from] Recoil\n|faint|p2b: Dragonite\n",
    );
    assert!(mons.iter().all(|m| m.direct_kos + m.passive_kos == 0), "{mons:#?}");
}
