use agave_feature_set::FeatureSet;
use solana_compute_budget::compute_budget::ComputeBudget;
use solana_fee_structure::FeeStructure;
use solana_instruction::Instruction;

use crate::accounts_db::AccountsDb;

pub struct Config {
    pub memoize: bool,
}

pub struct Seashell {
    pub config: Config,
    pub accounts_db: AccountsDb,
    pub compute_budget: ComputeBudget,
    pub fee_structure: FeeStructure,
    pub feature_set: FeatureSet,
}

impl Seashell {
    pub fn process_instruction(&self, _instruction: Instruction) {}

    pub fn benchmark_instruction(&self, _instruction: Instruction) {}

    pub fn process_instructions(&self, _instructions: Vec<Instruction>) {}

    pub fn benchmark_instructions(&self, _instructions: Vec<Instruction>) {}
}
