use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::Arc;

use inferno::flamegraph::{self, Options};
use object::{Object, ObjectSection, ObjectSymbol};
use solana_sbpf::ebpf::{hash_symbol_name, INSN_SIZE};
use solana_sbpf::profiler::{CuProfiler, FrameKey};

use crate::dwarf::{build_dwarf_index, DwarfIndex};

pub const SYSCALL_NAMES: &[&str] = &[
    "abort",
    "sol_alloc_free_",
    "sol_alt_bn128_compression",
    "sol_alt_bn128_group_op",
    "sol_big_mod_exp",
    "sol_blake3",
    "sol_create_program_address",
    "sol_curve_group_op",
    "sol_curve_multiscalar_mul",
    "sol_curve_validate_point",
    "sol_get_clock_sysvar",
    "sol_get_epoch_rewards_sysvar",
    "sol_get_epoch_schedule_sysvar",
    "sol_get_epoch_stake",
    "sol_get_fees_sysvar",
    "sol_get_last_restart_slot",
    "sol_get_processed_sibling_instruction",
    "sol_get_rent_sysvar",
    "sol_get_return_data",
    "sol_get_stack_height",
    "sol_get_sysvar",
    "sol_invoke_signed_c",
    "sol_invoke_signed_rust",
    "sol_keccak256",
    "sol_log_",
    "sol_log_64_",
    "sol_log_compute_units_",
    "sol_log_data",
    "sol_log_pubkey",
    "sol_memcmp_",
    "sol_memcpy_",
    "sol_memmove_",
    "sol_memset_",
    "sol_panic_",
    "sol_poseidon",
    "sol_remaining_compute_units",
    "sol_secp256k1_recover",
    "sol_set_return_data",
    "sol_sha256",
    "sol_try_find_program_address",
];

#[derive(Clone)]
pub struct Symbolicator {
    syscalls: HashMap<u32, &'static str>,
    functions: HashMap<u64, String>,
    dwarf: Option<Arc<DwarfIndex>>,
    text_base: u64,
}

impl Default for Symbolicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Symbolicator {
    pub fn new() -> Self {
        let syscalls = SYSCALL_NAMES
            .iter()
            .map(|n| (hash_symbol_name(n.as_bytes()), *n))
            .collect();
        Self {
            syscalls,
            functions: HashMap::new(),
            dwarf: None,
            text_base: 0,
        }
    }

    pub fn add_function(&mut self, entry_pc: u64, name: impl Into<String>) {
        self.functions.insert(entry_pc, name.into());
    }

    pub fn load_elf_symbols(&mut self, elf_bytes: &[u8]) -> Result<usize, String> {
        let file = object::File::parse(elf_bytes).map_err(|e| format!("parse ELF: {e}"))?;

        let Some(text) = file.section_by_name(".text") else {
            return Ok(0);
        };
        let text_base = text.address();
        let text_range = text_base..text_base.saturating_add(text.size());

        self.text_base = text_base;
        self.dwarf = build_dwarf_index(elf_bytes, text_base).map(Arc::new);

        let mut added = 0;
        for sym in file.symbols() {
            if sym.kind() != object::SymbolKind::Text || !text_range.contains(&sym.address()) {
                continue;
            }
            let Ok(name) = sym.name() else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let pc = sym.address().saturating_sub(text_base) / INSN_SIZE as u64;
            self.add_function(pc, rustc_demangle::demangle(name).to_string());
            added += 1;
        }
        Ok(added)
    }

    pub fn label(&self, key: FrameKey) -> String {
        match key {
            FrameKey::Syscall(k) => self
                .syscalls
                .get(&k)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("syscall@{k:#010x}")),
            FrameKey::Func(pc) => self
                .functions
                .get(&pc)
                .cloned()
                .unwrap_or_else(|| format!("fn@pc{pc}")),
            FrameKey::Program(id) => solana_pubkey::Pubkey::new_from_array(id).to_string(),
        }
    }

    pub fn folded(&self, profiler: &CuProfiler) -> Vec<String> {
        ProgramSymbolicators::new(self).folded(profiler)
    }

    pub fn has_dwarf(&self) -> bool {
        self.dwarf.is_some()
    }

    pub fn inline_frames(&self, pc: u64) -> Option<Vec<String>> {
        let index = self.dwarf.as_ref()?;
        let addr = pc.wrapping_mul(INSN_SIZE as u64).wrapping_add(self.text_base);
        let frames = index.frames_at(addr);
        if frames.is_empty() {
            return None;
        }
        Some(frames.into_iter().map(str::to_string).collect())
    }

    pub fn folded_inlined(&self, profiler: &CuProfiler) -> Vec<String> {
        ProgramSymbolicators::new(self).folded_inlined(profiler)
    }

    pub fn render_svg<W: io::Write>(
        &self,
        profiler: &CuProfiler,
        title: &str,
        writer: W,
    ) -> io::Result<()> {
        ProgramSymbolicators::new(self).render_svg(profiler, title, writer)
    }
}

pub struct ProgramSymbolicators<'a> {
    by_program: HashMap<[u8; 32], &'a Symbolicator>,
    fallback: &'a Symbolicator,
}

impl<'a> ProgramSymbolicators<'a> {
    pub fn new(fallback: &'a Symbolicator) -> Self {
        Self {
            by_program: HashMap::new(),
            fallback,
        }
    }

    pub fn add(&mut self, program_id: [u8; 32], symbolicator: &'a Symbolicator) {
        self.by_program.insert(program_id, symbolicator);
    }

    fn resolve(&self, owner: Option<[u8; 32]>) -> &Symbolicator {
        owner
            .and_then(|id| self.by_program.get(&id).copied())
            .unwrap_or(self.fallback)
    }

    fn owners(profiler: &CuProfiler) -> Vec<Option<[u8; 32]>> {
        let mut owners: Vec<Option<[u8; 32]>> = Vec::with_capacity(profiler.nodes.len());
        for node in &profiler.nodes {
            owners.push(match node.key {
                FrameKey::Program(id) => Some(id),
                _ => node.parent.and_then(|p| owners[p]),
            });
        }
        owners
    }

    pub fn folded(&self, profiler: &CuProfiler) -> Vec<String> {
        let mut lines: Vec<(String, u64)> = profiler
            .folded_stacks()
            .into_iter()
            .map(|(path, cu)| {
                let mut owner = None;
                let labels: Vec<String> = path
                    .into_iter()
                    .map(|k| {
                        if let FrameKey::Program(id) = k {
                            owner = Some(id);
                        }
                        self.resolve(owner).label(k)
                    })
                    .collect();
                (labels.join(";"), cu)
            })
            .collect();
        lines.sort_by(|a, b| b.1.cmp(&a.1));
        lines
            .into_iter()
            .map(|(stack, cu)| format!("{stack} {cu}"))
            .collect()
    }

    fn ancestor_frames(
        &self,
        profiler: &CuProfiler,
        idx: usize,
        owners: &[Option<[u8; 32]>],
    ) -> Vec<String> {
        let mut chain = vec![idx];
        while let Some(parent) = profiler.nodes[*chain.last().unwrap()].parent {
            chain.push(parent);
        }
        chain.reverse();

        let mut out = Vec::new();
        for pair in chain.windows(2) {
            let (ancestor, child) = (&profiler.nodes[pair[0]], &profiler.nodes[pair[1]]);
            let sym = self.resolve(owners[pair[0]]);
            let inlined = match (ancestor.key, child.callsite_pc) {
                (FrameKey::Func(_), Some(callsite_pc)) => sym.inline_frames(callsite_pc),
                _ => None,
            };
            match inlined {
                Some(frames) => out.extend(frames),
                None => out.push(sym.label(ancestor.key)),
            }
        }
        out
    }

    pub fn folded_inlined(&self, profiler: &CuProfiler) -> Vec<String> {
        let owners = Self::owners(profiler);
        let mut lines: BTreeMap<String, u64> = BTreeMap::new();
        for (idx, node) in profiler.nodes.iter().enumerate() {
            let sym = self.resolve(owners[idx]);
            let prefix = self.ancestor_frames(profiler, idx, &owners);
            match node.key {
                FrameKey::Func(_) => {
                    for (&pc, &cu) in &node.pc_cu {
                        let leaf = sym
                            .inline_frames(pc)
                            .unwrap_or_else(|| vec![sym.label(node.key)]);
                        let mut stack = prefix.clone();
                        stack.extend(leaf);
                        *lines.entry(stack.join(";")).or_insert(0) += cu;
                    }
                }
                FrameKey::Syscall(_) => {
                    if node.self_cu > 0 {
                        let mut stack = prefix.clone();
                        stack.push(sym.label(node.key));
                        *lines.entry(stack.join(";")).or_insert(0) += node.self_cu;
                    }
                }
                FrameKey::Program(_) => {}
            }
        }
        let mut out: Vec<(String, u64)> = lines.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out.into_iter()
            .map(|(stack, cu)| format!("{stack} {cu}"))
            .collect()
    }

    pub fn has_dwarf(&self) -> bool {
        self.fallback.has_dwarf() || self.by_program.values().any(|s| s.has_dwarf())
    }

    pub fn render_svg<W: io::Write>(
        &self,
        profiler: &CuProfiler,
        title: &str,
        writer: W,
    ) -> io::Result<()> {
        let lines = if self.has_dwarf() {
            self.folded_inlined(profiler)
        } else {
            self.folded(profiler)
        };
        let mut opts = Options::default();
        opts.title = title.to_string();
        opts.count_name = "CU".to_string();
        opts.subtitle = Some(format!("{} compute units", profiler.total_self_cu()));
        flamegraph::from_lines(&mut opts, lines.iter().map(String::as_str), writer)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}
