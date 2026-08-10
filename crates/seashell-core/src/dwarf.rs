use std::sync::Arc;

use gimli::Reader;
use object::{Object, ObjectSection};

type DwarfReader = gimli::EndianReader<gimli::RunTimeEndian, Arc<[u8]>>;

/// Fold a mangled DWARF address back into the text address space.
///
/// Below SBPFv3 every `.text` address fits in 32 bits, so the fold is a no-op
/// for real addresses and only rewrites the mangled ones. SBPFv3 moves `.text`
/// to MM_BYTECODE_START (0x1_0000_0000), where the fold corrupts EVERY address
/// — `0x1_0000_4600` becomes `0x4601` — and `build_dwarf_index` then discards
/// the DIE, and its whole subtree, as dead code. That silently strips the
/// inline frames out of a v3 profile: same pcs, same call tree, ~5x fewer
/// folded stacks.
///
/// An address already inside the text range is not mangled. Leave it alone.
fn unmangle_addr(x: u64, text_base: u64) -> u64 {
    if x >= text_base {
        return x;
    }
    (x >> 32).wrapping_add(x & 0xFFFF_FFFF)
}

struct DwarfFrame {
    low: u64,
    high: u64,
    depth: usize,
    name: String,
}

pub struct DwarfIndex {
    frames: Vec<DwarfFrame>,
}

impl DwarfIndex {
    pub(crate) fn frames_at(&self, addr: u64) -> Vec<&str> {
        let mut hits: Vec<&DwarfFrame> = self
            .frames
            .iter()
            .filter(|f| f.low <= addr && addr < f.high)
            .collect();
        hits.sort_by_key(|f| (f.depth, std::cmp::Reverse(f.high - f.low)));
        hits.into_iter().map(|f| f.name.as_str()).collect()
    }
}

fn attr_addr(
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    value: gimli::AttributeValue<DwarfReader>,
    text_base: u64,
) -> Option<u64> {
    match value {
        gimli::AttributeValue::Addr(a) => Some(unmangle_addr(a, text_base)),
        gimli::AttributeValue::DebugAddrIndex(i) => dwarf
            .address(unit, i)
            .ok()
            .map(|a| unmangle_addr(a, text_base)),
        _ => None,
    }
}

fn die_ranges_of(
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    debug_ranges: &[u8],
    entry: &gimli::DebuggingInformationEntry<DwarfReader>,
    text_base: u64,
) -> Vec<(u64, u64)> {
    if let Some(low) = entry
        .attr_value(gimli::DW_AT_low_pc)
        .ok()
        .flatten()
        .and_then(|v| attr_addr(dwarf, unit, v, text_base))
    {
        if let Ok(Some(hv)) = entry.attr_value(gimli::DW_AT_high_pc) {
            let high = match hv {
                gimli::AttributeValue::Udata(n) => Some(low.wrapping_add(n)),
                other => attr_addr(dwarf, unit, other, text_base),
            };
            return match high {
                Some(high) if high > low => vec![(low, high)],
                _ => Vec::new(),
            };
        }
    }
    if let Ok(Some(v)) = entry.attr_value(gimli::DW_AT_ranges) {
        if unit.header.version() < 5 {
            let offset = match v {
                gimli::AttributeValue::SecOffset(o) => o,
                gimli::AttributeValue::RangeListsRef(o) => o.0,
                other => {
                    log::debug!("DW_AT_ranges unhandled DWARF4 variant: {other:?}");
                    return Vec::new();
                }
            };
            return parse_debug_ranges(debug_ranges, offset, text_base);
        }
        let offset = match dwarf.attr_ranges_offset(unit, v) {
            Ok(Some(offset)) => offset,
            Ok(None) => return Vec::new(),
            Err(e) => {
                log::debug!("DW_AT_ranges offset resolution failed: {e}");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        match dwarf.ranges(unit, offset) {
            Ok(mut iter) => loop {
                match iter.next() {
                    Ok(Some(r)) => {
                        let (low, high) = (
                            unmangle_addr(r.begin, text_base),
                            unmangle_addr(r.end, text_base),
                        );
                        if high > low {
                            out.push((low, high));
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        log::debug!("rnglist iteration failed: {e}");
                        break;
                    }
                }
            },
            Err(e) => log::debug!("rnglist at {:#x} unreadable: {e}", offset.0),
        }
        return out;
    }
    Vec::new()
}

fn parse_debug_ranges(section: &[u8], offset: usize, text_base: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut base = 0u64;
    let mut pos = offset;
    while pos + 16 <= section.len() {
        let a = u64::from_le_bytes(section[pos..pos + 8].try_into().unwrap());
        let b = u64::from_le_bytes(section[pos + 8..pos + 16].try_into().unwrap());
        pos += 16;
        if a == 0 && b == 0 {
            break;
        }
        if a == u64::MAX {
            base = unmangle_addr(b, text_base);
            continue;
        }
        let low = base.wrapping_add(unmangle_addr(a, text_base));
        let high = base.wrapping_add(unmangle_addr(b, text_base));
        if high > low {
            out.push((low, high));
        }
    }
    out
}

fn die_name(
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    entry: &gimli::DebuggingInformationEntry<DwarfReader>,
    depth: u32,
) -> Option<String> {
    let attr_str = |at: gimli::DwAt| -> Option<String> {
        let value = entry.attr_value(at).ok().flatten()?;
        let s = dwarf.attr_string(unit, value).ok()?;
        Some(s.to_string_lossy().ok()?.into_owned())
    };
    if let Some(name) = attr_str(gimli::DW_AT_linkage_name) {
        return Some(rustc_demangle::demangle(&name).to_string());
    }
    if let Some(name) = attr_str(gimli::DW_AT_name) {
        return Some(rustc_demangle::demangle(&name).to_string());
    }
    if depth < 8 {
        for at in [gimli::DW_AT_abstract_origin, gimli::DW_AT_specification] {
            if let Some(gimli::AttributeValue::UnitRef(off)) = entry.attr_value(at).ok().flatten() {
                if let Ok(referenced) = unit.entry(off) {
                    if let Some(name) = die_name(dwarf, unit, &referenced, depth + 1) {
                        return Some(name);
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn build_dwarf_index(elf_bytes: &[u8], text_base: u64) -> Option<DwarfIndex> {
    let file = object::File::parse(elf_bytes).ok()?;
    file.section_by_name(".debug_info")?;
    let endian = if file.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let load = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
        let data = file
            .section_by_name(id.name())
            .and_then(|s| s.uncompressed_data().ok())
            .unwrap_or(std::borrow::Cow::Borrowed(&[]));
        Ok(gimli::EndianReader::new(Arc::from(data.as_ref()), endian))
    };
    let dwarf = gimli::Dwarf::load(load).ok()?;

    let debug_ranges: Vec<u8> = file
        .section_by_name(".debug_ranges")
        .and_then(|s| s.uncompressed_data().ok())
        .map(|d| d.into_owned())
        .unwrap_or_default();

    let mut frames = Vec::new();
    let (mut n_dies, mut n_ranged, mut n_named, mut n_dead) = (0u32, 0u32, 0u32, 0u32);
    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let Ok(unit) = dwarf.unit(header) else {
            continue;
        };
        let mut entries = unit.entries();
        let mut depth = 0isize;
        let mut frame_stack: Vec<isize> = Vec::new();
        let mut dead_at: Option<isize> = None;
        loop {
            let (delta, entry) = match entries.next_dfs() {
                Ok(Some(step)) => step,
                Ok(None) => break,
                Err(e) => {
                    log::debug!("DIE walk aborted mid-unit: {e}");
                    break;
                }
            };
            depth += delta;
            match dead_at {
                Some(d) if depth > d => continue,
                _ => dead_at = None,
            }
            while frame_stack.last().is_some_and(|&d| d >= depth) {
                frame_stack.pop();
            }
            if !matches!(
                entry.tag(),
                gimli::DW_TAG_subprogram | gimli::DW_TAG_inlined_subroutine
            ) {
                continue;
            }
            let frame_depth = frame_stack.len();
            frame_stack.push(depth);
            n_dies += 1;
            let ranges = die_ranges_of(&dwarf, &unit, &debug_ranges, entry, text_base);
            if ranges.is_empty() {
                continue;
            }
            if ranges.iter().any(|&(low, _)| low < text_base) {
                n_dead += 1;
                dead_at = Some(depth);
                frame_stack.pop();
                continue;
            }
            n_ranged += 1;
            if let Some(name) = die_name(&dwarf, &unit, entry, 0) {
                n_named += 1;
                for (low, high) in ranges {
                    frames.push(DwarfFrame {
                        low,
                        high,
                        depth: frame_depth,
                        name: name.clone(),
                    });
                }
            }
        }
    }
    log::debug!(
        "build_dwarf_index: {} frames (dies={n_dies} ranged={n_ranged} named={n_named} dead={n_dead})",
        frames.len()
    );
    Some(DwarfIndex { frames })
}
