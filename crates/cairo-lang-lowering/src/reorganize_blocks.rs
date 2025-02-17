use std::collections::HashMap;

use itertools::Itertools;

use crate::blocks::FlatBlocksBuilder;
use crate::borrow_check::analysis::{Analyzer, BackAnalysis, StatementLocation};
use crate::optimizations::remappings;
use crate::utils::{Rebuilder, RebuilderEx};
use crate::{
    BlockId, FlatBlock, FlatBlockEnd, FlatLowered, MatchInfo, Statement, VarRemapping, VarUsage,
    VariableId,
};

/// Reorganizes the blocks in lowered function and removes unnecessary remappings.
///
/// Removes unreachable blocks.
/// Blocks that are reachable only through goto are combined with the block that does the goto.
/// The order of the blocks is changed to be a topologically sorted.
pub fn reorganize_blocks(lowered: &mut FlatLowered) {
    if lowered.blocks.is_empty() {
        return;
    }
    let mut ctx = TopSortContext {
        old_block_rev_order: Default::default(),
        incoming_gotos: vec![0; lowered.blocks.len()],
        can_be_merged: vec![true; lowered.blocks.len()],
        remappings_ctx: Default::default(),
    };

    remappings::visit_remappings(lowered, |remapping| {
        for (dst, src) in remapping.iter() {
            ctx.remappings_ctx.dest_to_srcs.entry(*dst).or_default().push(src.var_id);
        }
    });

    let mut analysis =
        BackAnalysis { lowered: &*lowered, block_info: Default::default(), analyzer: ctx };
    analysis.get_root_info();
    let ctx = analysis.analyzer;

    // Rebuild the blocks in the correct order.
    let mut new_blocks = FlatBlocksBuilder::default();

    // Keep only blocks that can't be merged or have more than 1 incoming
    // goto.
    // Note that unreachable block were not added to `ctx.old_block_rev_order` during
    // the analysis above.
    let mut old_block_rev_order = ctx
        .old_block_rev_order
        .into_iter()
        .filter(|block_id| !ctx.can_be_merged[block_id.0] || ctx.incoming_gotos[block_id.0] > 1)
        .collect_vec();

    // Add the root block as it was filtered above.
    old_block_rev_order.push(BlockId::root());

    let n_visited_blocks = old_block_rev_order.len();

    let mut rebuilder = RebuildContext {
        block_remapping: HashMap::from_iter(
            old_block_rev_order
                .iter()
                .enumerate()
                .map(|(idx, block_id)| (*block_id, BlockId(n_visited_blocks - idx - 1))),
        ),
        remappings_ctx: ctx.remappings_ctx,
    };
    for block_id in old_block_rev_order.into_iter().rev() {
        let mut statements = vec![];

        let mut block = &lowered.blocks[block_id];
        loop {
            for stmt in &block.statements {
                statements.push(rebuilder.rebuild_statement(stmt));
            }
            if let FlatBlockEnd::Goto(target_block_id, remappings) = &block.end {
                if rebuilder.block_remapping.get(target_block_id).is_none() {
                    assert!(
                        rebuilder.rebuild_remapping(remappings).is_empty(),
                        "Remapping should be empty."
                    );
                    block = &lowered.blocks[*target_block_id];
                    continue;
                }
            }
            break;
        }

        let end = rebuilder.rebuild_end(&block.end);
        new_blocks.alloc(FlatBlock { statements, end });
    }

    lowered.blocks = new_blocks.build().unwrap();
}

pub struct TopSortContext {
    old_block_rev_order: Vec<BlockId>,
    // The number of incoming gotos, indexed by block_id.
    incoming_gotos: Vec<usize>,

    // True if the block can be merged with the block that goes to it.
    can_be_merged: Vec<bool>,

    remappings_ctx: remappings::Context,
}

impl Analyzer<'_> for TopSortContext {
    type Info = ();

    fn visit_block_start(&mut self, _info: &mut Self::Info, block_id: BlockId, _block: &FlatBlock) {
        self.old_block_rev_order.push(block_id);
    }

    fn visit_stmt(
        &mut self,
        _info: &mut Self::Info,
        _statement_location: StatementLocation,
        stmt: &Statement,
    ) {
        for var_usage in stmt.inputs() {
            self.remappings_ctx.set_used(var_usage.var_id);
        }
    }

    fn visit_goto(
        &mut self,
        _info: &mut Self::Info,
        _statement_location: StatementLocation,
        target_block_id: BlockId,
        remapping: &VarRemapping,
    ) {
        self.incoming_gotos[target_block_id.0] += 1;

        for var_usage in remapping.values() {
            self.remappings_ctx.set_used(var_usage.var_id);
        }
    }

    fn merge_match(
        &mut self,
        _statement_location: StatementLocation,
        match_info: &MatchInfo,
        _infos: &[Self::Info],
    ) -> Self::Info {
        for var_usage in match_info.inputs() {
            self.remappings_ctx.set_used(var_usage.var_id);
        }

        for arm in match_info.arms().iter() {
            self.can_be_merged[arm.block_id.0] = false;
        }
    }

    fn info_from_return(
        &mut self,
        _statement_location: StatementLocation,
        vars: &[VarUsage],
    ) -> Self::Info {
        for var_usage in vars {
            self.remappings_ctx.set_used(var_usage.var_id);
        }
    }

    fn info_from_panic(
        &mut self,
        _statement_location: StatementLocation,
        data: &VarUsage,
    ) -> Self::Info {
        self.remappings_ctx.set_used(data.var_id);
    }
}

pub struct RebuildContext {
    block_remapping: HashMap<BlockId, BlockId>,
    remappings_ctx: remappings::Context,
}
impl Rebuilder for RebuildContext {
    fn map_block_id(&mut self, block: BlockId) -> BlockId {
        self.block_remapping[&block]
    }

    fn map_var_id(&mut self, var: VariableId) -> VariableId {
        self.remappings_ctx.map_var_id(var)
    }

    fn transform_remapping(&mut self, remapping: &mut VarRemapping) {
        self.remappings_ctx.transform_remapping(remapping)
    }
}
