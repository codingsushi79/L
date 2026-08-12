//! MIR optimisation (SPEC §80, §82).
//!
//! The passes here are the ones that pay for themselves regardless of what the
//! backend does with the result: they shrink the code the backend has to walk
//! and remove work the program would otherwise do at run time. Machine-level
//! optimisation is left to the C compiler behind the backend (SPEC §82).
//!
//! Every pass preserves observable behaviour, including failures: constant
//! folding never folds away a division by zero, because that division is
//! required to fail (SPEC §31).

use l_hir::{BinOp, Ty, UnOp};
use l_mir::{BasicBlock, BlockId, Body, Const, Operand, Program, Rvalue, Stmt, Terminator};

/// How hard to optimise.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Level {
    /// Only the passes that also make debugging easier.
    #[default]
    None,
    /// Everything in this crate.
    Full,
}

/// A count of what each pass did, for `--verbose` builds and for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub folded: usize,
    pub blocks_removed: usize,
    pub branches_resolved: usize,
}

/// Optimise a whole program in place.
pub fn optimise(program: &mut Program, level: Level) -> Stats {
    let mut stats = Stats::default();
    for body in &mut program.bodies {
        optimise_body(body, level, &mut stats);
    }
    stats
}

fn optimise_body(body: &mut Body, level: Level, stats: &mut Stats) {
    if level == Level::Full {
        fold_constants(body, stats);
        resolve_branches(body, stats);
    }
    // Unreachable blocks are dropped at every level: they are never useful to
    // the backend, and a block left over from a `return` in the middle of a
    // function is common enough to be worth removing always.
    remove_unreachable(body, stats);
}

// ===========================================================================
// Constant folding (SPEC §8: constants are evaluated at compile time)
// ===========================================================================

fn fold_constants(body: &mut Body, stats: &mut Stats) {
    for block in &mut body.blocks {
        for stmt in &mut block.stmts {
            let rv = match stmt {
                Stmt::Assign(_, rv) | Stmt::Eval(rv) => rv,
                _ => continue,
            };
            if let Some(folded) = fold(rv) {
                *rv = Rvalue::Use(Operand::Const(folded));
                stats.folded += 1;
            }
        }
    }
}

/// Evaluate an rvalue whose operands are all constants, where doing so cannot
/// change what the program does.
fn fold(rv: &Rvalue) -> Option<Const> {
    match rv {
        Rvalue::Unary(op, Operand::Const(c), ty) => fold_unary(*op, c, ty),
        Rvalue::Binary(op, Operand::Const(a), Operand::Const(b), ty) => fold_binary(*op, a, b, ty),
        Rvalue::Concat(parts) => {
            let mut out = String::new();
            for p in parts {
                match p {
                    Operand::Const(Const::Str(s)) => out.push_str(s),
                    _ => return None,
                }
            }
            Some(Const::Str(out))
        }
        _ => None,
    }
}

fn fold_unary(op: UnOp, c: &Const, ty: &Ty) -> Option<Const> {
    match (op, c) {
        (UnOp::Neg, Const::Int(v)) => v.checked_neg().map(Const::Int),
        (UnOp::Neg, Const::Float(v)) => Some(Const::Float(-v)),
        (UnOp::Not, Const::Bool(v)) => Some(Const::Bool(!v)),
        // Widths below 64 bits would need masking to fold correctly, so only
        // the native width is folded here.
        (UnOp::BitNot, Const::Int(v)) if matches!(ty, Ty::Prim(p) if p.bit_width() == Some(64)) => {
            Some(Const::Int(!v))
        }
        _ => None,
    }
}

fn fold_binary(op: BinOp, a: &Const, b: &Const, ty: &Ty) -> Option<Const> {
    use BinOp::*;

    // Only the native integer width folds; narrower types would need to be
    // truncated, and this pass would have to know the target's rules.
    let native_int = matches!(ty, Ty::Prim(p) if p.is_integer() && p.bit_width() == Some(64));

    match (a, b) {
        (Const::Int(x), Const::Int(y)) if native_int => match op {
            Add => x.checked_add(*y).map(Const::Int),
            Sub => x.checked_sub(*y).map(Const::Int),
            Mul => x.checked_mul(*y).map(Const::Int),
            // A division by zero must still fail at run time (SPEC §31), so
            // it is deliberately left alone.
            Div if *y != 0 => x.checked_div(*y).map(Const::Int),
            Rem if *y != 0 => x.checked_rem(*y).map(Const::Int),
            BitAnd => Some(Const::Int(x & y)),
            BitOr => Some(Const::Int(x | y)),
            BitXor => Some(Const::Int(x ^ y)),
            Shl if (0..64).contains(y) => Some(Const::Int(x << y)),
            Shr if (0..64).contains(y) => Some(Const::Int(x >> y)),
            Eq => Some(Const::Bool(x == y)),
            Ne => Some(Const::Bool(x != y)),
            Lt => Some(Const::Bool(x < y)),
            Le => Some(Const::Bool(x <= y)),
            Gt => Some(Const::Bool(x > y)),
            Ge => Some(Const::Bool(x >= y)),
            _ => None,
        },

        (Const::Float(x), Const::Float(y)) => match op {
            Add => Some(Const::Float(x + y)),
            Sub => Some(Const::Float(x - y)),
            Mul => Some(Const::Float(x * y)),
            Div if *y != 0.0 => Some(Const::Float(x / y)),
            Eq => Some(Const::Bool(x == y)),
            Ne => Some(Const::Bool(x != y)),
            Lt => Some(Const::Bool(x < y)),
            Le => Some(Const::Bool(x <= y)),
            Gt => Some(Const::Bool(x > y)),
            Ge => Some(Const::Bool(x >= y)),
            _ => None,
        },

        (Const::Bool(x), Const::Bool(y)) => match op {
            Eq => Some(Const::Bool(x == y)),
            Ne => Some(Const::Bool(x != y)),
            BitAnd => Some(Const::Bool(*x && *y)),
            BitOr => Some(Const::Bool(*x || *y)),
            _ => None,
        },

        (Const::Str(x), Const::Str(y)) => match op {
            Eq => Some(Const::Bool(x == y)),
            Ne => Some(Const::Bool(x != y)),
            Lt => Some(Const::Bool(x < y)),
            Le => Some(Const::Bool(x <= y)),
            Gt => Some(Const::Bool(x > y)),
            Ge => Some(Const::Bool(x >= y)),
            Add => Some(Const::Str(format!("{x}{y}"))),
            _ => None,
        },

        _ => None,
    }
}

// ===========================================================================
// Branch resolution
// ===========================================================================

/// Turn a branch on a known condition into a jump. This is what removes the
/// dead half of `if false { ... }` and of the guards that desugaring inserts.
fn resolve_branches(body: &mut Body, stats: &mut Stats) {
    for block in &mut body.blocks {
        let replacement = match &block.term {
            Terminator::If { cond: Operand::Const(Const::Bool(v)), then, els } => {
                Some(Terminator::Goto(if *v { *then } else { *els }))
            }
            Terminator::Switch { value: Operand::Const(Const::Int(v)), targets, default } => {
                let target = targets
                    .iter()
                    .find(|(k, _)| k == v)
                    .map(|(_, b)| *b)
                    .unwrap_or(*default);
                Some(Terminator::Goto(target))
            }
            _ => None,
        };
        if let Some(t) = replacement {
            block.term = t;
            stats.branches_resolved += 1;
        }
    }
}

// ===========================================================================
// Unreachable block removal
// ===========================================================================

fn remove_unreachable(body: &mut Body, stats: &mut Stats) {
    let live = reachable(body);
    if live.iter().all(|&x| x) {
        return;
    }

    // Blocks keep their relative order, so the remaining indices are stable
    // and the generated C stays readable.
    let mut mapping: Vec<Option<BlockId>> = vec![None; body.blocks.len()];
    let mut next = 0u32;
    for (i, &alive) in live.iter().enumerate() {
        if alive {
            mapping[i] = Some(BlockId(next));
            next += 1;
        }
    }

    let old = std::mem::take(&mut body.blocks);
    let mut kept: Vec<BasicBlock> = Vec::with_capacity(next as usize);
    for (i, block) in old.into_iter().enumerate() {
        if !live[i] {
            stats.blocks_removed += 1;
            continue;
        }
        kept.push(block);
    }

    for block in &mut kept {
        remap(&mut block.term, &mapping);
    }
    body.blocks = kept;
}

fn reachable(body: &Body) -> Vec<bool> {
    let mut live = vec![false; body.blocks.len()];
    if body.blocks.is_empty() {
        return live;
    }
    let mut stack = vec![Body::ENTRY];
    while let Some(id) = stack.pop() {
        let idx = id.0 as usize;
        if idx >= live.len() || live[idx] {
            continue;
        }
        live[idx] = true;
        for succ in body.blocks[idx].term.successors() {
            stack.push(succ);
        }
    }
    live
}

fn remap(term: &mut Terminator, mapping: &[Option<BlockId>]) {
    let to = |b: &mut BlockId| {
        if let Some(Some(new)) = mapping.get(b.0 as usize) {
            *b = *new;
        }
    };
    match term {
        Terminator::Goto(b) => to(b),
        Terminator::If { then, els, .. } => {
            to(then);
            to(els);
        }
        Terminator::Switch { targets, default, .. } => {
            for (_, b) in targets.iter_mut() {
                to(b);
            }
            to(default);
        }
        Terminator::Try { handler, body } => {
            to(handler);
            to(body);
        }
        Terminator::Return | Terminator::Unreachable => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l_hir::Prim;
    use l_mir::Place;
    use l_span::Span;

    fn int_ty() -> Ty {
        Ty::Prim(Prim::Int)
    }

    fn body_with(blocks: Vec<BasicBlock>) -> Body {
        Body {
            def: l_hir::DefId(0),
            name: "f".into(),
            qualified: "m.f".into(),
            params: vec![],
            ret: Ty::Void,
            locals: vec![],
            blocks,
            is_extern: false,
            is_variadic: false,
            is_test: false,
            is_benchmark: false,
            span: Span::dummy(),
        }
    }

    #[test]
    fn folds_integer_arithmetic() {
        let rv = Rvalue::Binary(
            BinOp::Add,
            Operand::Const(Const::Int(2)),
            Operand::Const(Const::Int(3)),
            int_ty(),
        );
        assert_eq!(fold(&rv), Some(Const::Int(5)));
    }

    #[test]
    fn does_not_fold_division_by_zero() {
        // The division must still happen, and still fail (SPEC §31).
        let rv = Rvalue::Binary(
            BinOp::Div,
            Operand::Const(Const::Int(1)),
            Operand::Const(Const::Int(0)),
            int_ty(),
        );
        assert_eq!(fold(&rv), None);
    }

    #[test]
    fn does_not_fold_overflow() {
        let rv = Rvalue::Binary(
            BinOp::Add,
            Operand::Const(Const::Int(i128::MAX)),
            Operand::Const(Const::Int(1)),
            int_ty(),
        );
        assert_eq!(fold(&rv), None);
    }

    #[test]
    fn folds_comparisons_and_strings() {
        let cmp = Rvalue::Binary(
            BinOp::Lt,
            Operand::Const(Const::Int(1)),
            Operand::Const(Const::Int(2)),
            int_ty(),
        );
        assert_eq!(fold(&cmp), Some(Const::Bool(true)));

        let cat = Rvalue::Concat(vec![
            Operand::Const(Const::Str("Hello, ".into())),
            Operand::Const(Const::Str("world".into())),
        ]);
        assert_eq!(fold(&cat), Some(Const::Str("Hello, world".into())));
    }

    #[test]
    fn leaves_narrow_integer_types_alone() {
        // Folding an 8-bit add here would need target truncation rules.
        let rv = Rvalue::Binary(
            BinOp::Add,
            Operand::Const(Const::Int(200)),
            Operand::Const(Const::Int(200)),
            Ty::Prim(Prim::Int8),
        );
        assert_eq!(fold(&rv), None);
    }

    #[test]
    fn resolves_constant_branches_and_drops_the_dead_half() {
        let blocks = vec![
            BasicBlock {
                stmts: vec![],
                term: Terminator::If {
                    cond: Operand::Const(Const::Bool(true)),
                    then: BlockId(1),
                    els: BlockId(2),
                },
            },
            BasicBlock { stmts: vec![], term: Terminator::Return },
            // Only reachable through the false arm.
            BasicBlock { stmts: vec![Stmt::Nop], term: Terminator::Return },
        ];
        let mut body = body_with(blocks);
        let mut stats = Stats::default();
        resolve_branches(&mut body, &mut stats);
        remove_unreachable(&mut body, &mut stats);

        assert_eq!(stats.branches_resolved, 1);
        assert_eq!(stats.blocks_removed, 1);
        assert_eq!(body.blocks.len(), 2);
        assert!(matches!(body.blocks[0].term, Terminator::Goto(BlockId(1))));
    }

    #[test]
    fn keeps_every_reachable_block() {
        let blocks = vec![
            BasicBlock { stmts: vec![], term: Terminator::Goto(BlockId(1)) },
            BasicBlock { stmts: vec![], term: Terminator::Goto(BlockId(2)) },
            BasicBlock { stmts: vec![], term: Terminator::Return },
        ];
        let mut body = body_with(blocks);
        let mut stats = Stats::default();
        remove_unreachable(&mut body, &mut stats);
        assert_eq!(body.blocks.len(), 3);
        assert_eq!(stats.blocks_removed, 0);
    }

    #[test]
    fn remapping_keeps_branch_targets_correct() {
        // Block 1 is dead; block 2 must be renumbered to 1 and the jump in
        // block 0 must follow it.
        let blocks = vec![
            BasicBlock { stmts: vec![], term: Terminator::Goto(BlockId(2)) },
            BasicBlock { stmts: vec![], term: Terminator::Return },
            BasicBlock {
                stmts: vec![Stmt::Assign(
                    Place::local(l_hir::LocalId(0)),
                    Rvalue::Use(Operand::Const(Const::Int(1))),
                )],
                term: Terminator::Return,
            },
        ];
        let mut body = body_with(blocks);
        let mut stats = Stats::default();
        remove_unreachable(&mut body, &mut stats);

        assert_eq!(body.blocks.len(), 2);
        assert!(matches!(body.blocks[0].term, Terminator::Goto(BlockId(1))));
        assert_eq!(stats.blocks_removed, 1);
    }
}
