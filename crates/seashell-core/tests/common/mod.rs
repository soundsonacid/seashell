#![allow(dead_code)]

use std::path::{Path, PathBuf};

use solana_pubkey::Pubkey;

pub const TOKEN_ACCOUNT_RENT: u64 = 2_039_280;

pub fn spl_token_account(mint: &Pubkey, owner: &Pubkey, amount: u64, native: bool) -> Vec<u8> {
    let mut data = vec![0u8; 165];
    data[0..32].copy_from_slice(mint.as_ref());
    data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());
    data[108] = 1;
    if native {
        data[109..113].copy_from_slice(&1u32.to_le_bytes());
        data[113..121].copy_from_slice(&TOKEN_ACCOUNT_RENT.to_le_bytes());
    }
    data
}

pub fn quasar_profile_dirs() -> Vec<PathBuf> {
    let cargo_home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").expect("HOME")).join(".cargo")
    });
    let mut dirs = Vec::new();
    let Ok(checkouts) = std::fs::read_dir(cargo_home.join("git/checkouts")) else {
        return dirs;
    };
    for repo in checkouts.flatten() {
        if !repo.file_name().to_string_lossy().starts_with("quasar-") {
            continue;
        }
        let Ok(revs) = std::fs::read_dir(repo.path()) else {
            continue;
        };
        for rev in revs.flatten() {
            let profiles = rev.path().join("profile/profiles");
            if profiles.is_dir() {
                dirs.push(profiles);
            }
        }
    }
    dirs
}

pub fn latest_profile_json(name: &str) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in quasar_profile_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.starts_with(&format!("{name}__")) || !file_name.ends_with(".profile.json")
            {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if best.as_ref().is_none_or(|(t, _)| modified > *t) {
                best = Some((modified, entry.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn folded_lines(node: &serde_json::Value, path: &mut Vec<String>, out: &mut Vec<String>) {
    let name = node["name"].as_str().unwrap_or("?");
    let value = node["value"].as_u64().unwrap_or(0);
    let children = node["children"].as_array().cloned().unwrap_or_default();
    let child_sum: u64 = children
        .iter()
        .map(|c| c["value"].as_u64().unwrap_or(0))
        .sum();
    let is_root = path.is_empty() && name == "all";
    if !is_root {
        path.push(name.to_string());
    }
    let self_cu = value.saturating_sub(child_sum);
    if self_cu > 0 && !path.is_empty() {
        out.push(format!("{} {}", path.join(";"), self_cu));
    }
    for child in &children {
        folded_lines(child, path, out);
    }
    if !is_root {
        path.pop();
    }
}

pub fn render_quasar_svg(json_path: &Path, title: &str, out_path: &Path) -> u64 {
    let profile: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(json_path).unwrap()).unwrap();
    let total = profile["root"]["value"].as_u64().unwrap_or(0);
    let mut lines = Vec::new();
    folded_lines(&profile["root"], &mut Vec::new(), &mut lines);
    let mut opts = inferno::flamegraph::Options::default();
    opts.title = title.to_string();
    opts.count_name = "CU".to_string();
    opts.subtitle = Some(format!("{total} static code CU (quasar)"));
    let file = std::fs::File::create(out_path).unwrap();
    inferno::flamegraph::from_lines(&mut opts, lines.iter().map(String::as_str), file)
        .expect("render quasar flamegraph");
    total
}
