#![feature(random)]
#![feature(return_type_notation)]
// Cargo's `[lints.clippy]` allow-list is overwritten by the `pedantic`/ `nursery` group flags on this
// nightly (lint-groups-priority).
#![allow(
    clippy::assert_is_empty,
    clippy::assertions_on_constants,
    clippy::assigning_clones,
    // Windows FFI pointer casts left for #17's `&raw` migration; the
    // `ref_as_ptr` class was fixed here with `std::ptr::from_{mut,ref}`.
    clippy::borrow_as_ptr,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::bool_to_int_with_if,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::default_trait_access,
    clippy::derive_partial_eq_without_eq,
    clippy::doc_markdown,
    clippy::duration_suboptimal_units,
    clippy::equatable_if_let,
    clippy::explicit_iter_loop,
    clippy::format_push_string,
    clippy::future_not_send,
    clippy::if_not_else,
    clippy::ignored_unit_patterns,
    clippy::implicit_clone,
    clippy::implicit_hasher,
    clippy::items_after_statements,
    clippy::large_futures,
    clippy::lint_groups_priority,
    clippy::manual_assert,
    clippy::manual_is_variant_and,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::naive_bytecount,
    clippy::needless_collect,
    clippy::needless_continue,
    clippy::needless_pass_by_value,
    clippy::needless_pass_by_ref_mut,
    clippy::nonminimal_bool,
    clippy::option_if_let_else,
    clippy::option_option,
    clippy::or_fun_call,
    clippy::redundant_clone,
    clippy::redundant_closure_for_method_calls,
    clippy::ref_option,
    clippy::return_self_not_must_use,
    clippy::semicolon_if_nothing_returned,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::single_match_else,
    clippy::single_option_map,
    clippy::significant_drop_tightening,
    clippy::stable_sort_primitive,
    clippy::struct_excessive_bools,
    clippy::struct_field_names,
    clippy::suboptimal_flops,
    clippy::suspicious_operation_groupings,
    clippy::too_long_first_doc_paragraph,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::unchecked_time_subtraction,
    clippy::uninlined_format_args,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnested_or_patterns,
    clippy::unreadable_literal,
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::unused_peekable,
    clippy::unused_self,
    clippy::use_self,
    clippy::useless_let_if_seq,
    clippy::verbose_bit_mask,
    clippy::wildcard_imports
)]
//! kelir — a small, steerable coding-agent harness.
//! One session is one OS process plus one append-only event log; every request is rebuilt
//! from `fold(events.jsonl)` so the loop is the same live and on resume.

pub mod config;
pub mod core;
pub mod docs;
pub mod envs;
pub mod features;
pub mod ipc;
pub mod log;
pub mod main_cli;
pub mod paths;
pub mod provider;
pub mod sys;
pub mod tools;
pub mod tui;
pub mod util;

/// The crate version, as reported to providers and in the event log.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
