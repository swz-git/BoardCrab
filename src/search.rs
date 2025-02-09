use std::collections::HashSet;
use crate::board::*;
use crate::eval::*;
use crate::move_gen;
use crate::zobrist::Hash;
use crate::move_gen::MoveBuffer;
use crate::transpos;
use crate::thread_flag::ThreadFlag;

fn _perft(board: &Board, depth: usize, depth_elapsed: usize, print: bool) -> usize {
    let mut moves = move_gen::MoveBuffer::new();
    move_gen::generate_moves(&board, &mut moves);
    if depth > 1 {
        let mut total: usize = 0;
        for mv in moves.iter() {
            let mut next_board: Board = *board;
            next_board.do_move(&mv);
            let sub_total = _perft(&next_board, depth - 1, depth_elapsed + 1, print);
            if depth_elapsed == 0 && print {
                println!("{}: {}", mv, sub_total);
            }
            total += sub_total;
        }
        if depth_elapsed == 0 && print {
            println!("\nNodes Searched: {}", total);
        }
        total
    } else if depth == 1 {
        if depth_elapsed == 0 && print {
            for mv in moves.iter() {
                println!("{}: {}", mv, 1);
            }
            println!("\nNodes Searched: {}", moves.len());
        }
        moves.len()
    } else {
        if depth_elapsed == 0 && print {
            println!("\nNodes Searched: 1");
        }
        return 1;
    }
}

pub fn perft(board: &Board, depth: usize, print: bool) -> usize { _perft(board, depth, 0, print) }

//////////////////////////////////////////////////////////////////////////

fn get_no_moves_eval(board: &Board) -> Value {
    if board.checkers != 0 { -VALUE_CHECKMATE } else { 0.0 }
}

fn is_extending_move(mv: &Move) -> bool {
    mv.has_flag(Move::FLAG_CAPTURE) || mv.has_flag(Move::FLAG_PROMOTION)
}

// Maximum depth to extend searches to
const MAX_EXTENSION_DEPTH: usize = 4;

// Searches only extending moves
fn extension_search(board: &Board, search_info: &mut SearchInfo, mut lower_bound: Value, upper_bound: Value, depth_remaining: usize) -> Value {
    search_info.total_nodes += 1;

    // Ref: https://www.chessprogramming.org/Quiescence_Search
    let mut best_eval = eval_board(board);

    if best_eval >= upper_bound {
        return best_eval;
    } else if best_eval > lower_bound {
        lower_bound = best_eval;
    }

    if depth_remaining == 0 {
        return best_eval;
    }

    let mut moves = move_gen::MoveBuffer::new();
    move_gen::generate_moves(board, &mut moves);
    if moves.is_empty() {
        return get_no_moves_eval(board);
    }
    for mv in moves.iter() {
        if !is_extending_move(mv) {
            continue
        }

        let mut next_board = board.clone();
        next_board.do_move(mv);
        let base_next_eval = extension_search(&next_board, search_info, -upper_bound, -lower_bound, depth_remaining - 1);
        let next_eval = decay_eval(-base_next_eval);

        if next_eval > best_eval {
            best_eval = next_eval;

            if next_eval > lower_bound {
                lower_bound = best_eval
            }
            if next_eval >= upper_bound {
                break;
            }
        }
    }

    best_eval
}

pub struct SearchInfo {
    pub total_nodes: usize,
    pub depth_hashes: [Hash; 256] // For repetition detection
}

impl SearchInfo {
    pub fn new() -> SearchInfo {
        SearchInfo {
            total_nodes: 0,
            depth_hashes: [0; 256]
        }
    }
}

pub struct SearchResult {
    pub eval: Value,
    pub best_move_idx: Option<usize> // May not exist if at depth 0
}

fn _search(
    board: &Board, table: &mut transpos::Table, search_info: &mut SearchInfo,
    mut lower_bound: Value, upper_bound: Value,
    depth_remaining: u8, depth_elapsed: u8,
    stop_flag: &ThreadFlag, stop_time: Option<std::time::Instant>) -> SearchResult {

    search_info.total_nodes += 1;

    // Check draw by repetition
    for i in (4..10).step_by(2) {
        if (depth_elapsed >= i) && search_info.depth_hashes[(depth_elapsed - i) as usize] == board.hash {
            // Loop detected
            return SearchResult {
                eval: 0.0,
                best_move_idx: None
            };
        } else {
            break;
        }
    }
    search_info.depth_hashes[depth_elapsed as usize] = board.hash;

    if depth_remaining > 2 { // No point in checking at a super low depth
        let mut stop = false;

        if stop_flag.get() {
            stop = true;
        } else if stop_time.is_some() {
            // TODO: Is this slow?
            if std::time::Instant::now() >= stop_time.unwrap() {
                stop = true
            }
        }

        if stop {
            return SearchResult {
                eval: VALUE_INF,
                best_move_idx: None
            };
        }
    }

    let table_entry = table.get(board.hash);

    let mut table_best_move: Option<u8> = None;
    if table_entry.is_valid() {
        if table_entry.depth_remaining >= depth_remaining {
            match table_entry.entry_type {
                transpos::EntryType::Exact | transpos::EntryType::LowerBound => {
                    // TODO: Does it actually matter if it's the lower bound or not?
                    return SearchResult {
                        eval: table_entry.eval,
                        best_move_idx: Some(table_entry.best_move_idx as usize)
                    };
                },
                transpos::EntryType::UpperBound => {
                    if table_entry.eval >= upper_bound {
                        // Beta cut-off
                        return SearchResult {
                            eval: table_entry.eval,
                            best_move_idx: Some(table_entry.best_move_idx as usize)
                        };
                    } else {
                        // Just use the best move
                        table_best_move = Some(table_entry.best_move_idx);
                    }
                },
                _ => {
                    panic!("Invalid or unsupported entry type: {}", table_entry.entry_type as usize);
                }
            }
        } else {
            // From a lower depth, so not super useful
            // However, we can still use the best move
            table_best_move = Some(table_entry.best_move_idx);
        }
    }

    if depth_remaining > 0 {
        let mut moves = MoveBuffer::new();
        move_gen::generate_moves(&board, &mut moves);
        if moves.is_empty() {
            return SearchResult {
                eval: get_no_moves_eval(board),
                best_move_idx: None
            }
        }

        #[derive(Copy, Clone)]
        struct RatedMove {
            idx: usize,
            eval: Value
        }

        let mut rated_moves: Vec<RatedMove> = Vec::with_capacity(moves.len());
        for i in 0..moves.len() {
            rated_moves.push(
                RatedMove {
                    idx: i,
                    eval: eval_move(board, &moves[i]),
                }
            )
        }

        if table_best_move.is_some() {
            rated_moves[table_best_move.unwrap() as usize].eval = VALUE_INF;
        }

        // Insertion sort
        for i in 1..moves.len() {
            let mut j = i;
            while j > 0 {
                let prev = rated_moves[j - 1];
                let cur = rated_moves[j];

                if cur.eval > prev.eval {
                    // Swap
                    rated_moves[j - 1] = cur;
                    rated_moves[j] = prev;
                } else {
                    break
                }

                j -= 1;
            }
        }

        let mut best_eval = -VALUE_INF;
        let mut best_move_idx: usize = 0;
        let mut upper_bound_hit = false;
        for i in 0..moves.len() {
            let move_idx = rated_moves[i].idx;
            let mv = &moves[move_idx];

            let mut next_board: Board = board.clone();
            next_board.do_move(mv);

            let next_result = _search(
                    &next_board, table, search_info,
                    -upper_bound, -lower_bound,
                    depth_remaining - 1, depth_elapsed + 1,
                    stop_flag, stop_time
            );

            if next_result.eval == VALUE_INF {
                // Search aborted
                return SearchResult {
                    eval: VALUE_INF,
                    best_move_idx: None
                }
            }

            let next_eval = decay_eval(-next_result.eval);
            if next_eval > best_eval {
                best_eval = next_eval;
                best_move_idx = move_idx;
                if next_eval > lower_bound {
                    lower_bound = next_eval;
                }

                if next_eval >= upper_bound {
                    // Beta cut-off
                    upper_bound_hit = true;
                    break
                }
            }
        }

        table.set(
            transpos::Entry {
                hash: board.hash,
                eval: best_eval,
                best_move_idx: best_move_idx as u8,
                depth_remaining,
                entry_type: if upper_bound_hit { transpos::EntryType::UpperBound } else { transpos::EntryType::Exact },
                age_count: 0 // Will be set by the transposition table
            }
        );

        SearchResult {
            eval: best_eval,
            best_move_idx: Some(best_move_idx)
        }

    } else {
        SearchResult {
            eval: extension_search(board, search_info, lower_bound, upper_bound, MAX_EXTENSION_DEPTH),
            best_move_idx: None
        }
    }
}

pub fn search(
    board: &Board, table: &mut transpos::Table, depth: u8,
    stop_flag: &ThreadFlag, stop_time: Option<std::time::Instant>) -> (SearchResult, SearchInfo) {

    let mut search_info = SearchInfo::new();
    let search_result = _search(
        board, table, &mut search_info, -VALUE_CHECKMATE, VALUE_CHECKMATE, depth, 0, stop_flag, stop_time
    );

    (search_result, search_info)
}

pub fn determine_pv(mut board: Board, table: &transpos::Table) -> Vec<Move> {
    let mut result = Vec::new();
    let mut found_hashes = HashSet::<Hash>::new();

    loop {

        let entry = table.get(board.hash);
        if entry.hash == board.hash {
            if found_hashes.contains(&board.hash) {
                // Looped position
                break;
            } else {
                found_hashes.insert(board.hash);
            }

            let mut moves = MoveBuffer::new();
            move_gen::generate_moves(&board, &mut moves);

            let best_move_idx = entry.best_move_idx as usize;
            if best_move_idx >= moves.len() {
                panic!("Failed to generate PV, bad move count (hash collision?)");
            }

            let best_move = moves[best_move_idx];
            result.push(best_move);
            board.do_move(&best_move);
        } else {
            break;
        }
    }

    if result.is_empty() {
        panic!("Failed to generate PV, first table entry never found");
    }

    result
}