// C++ declarations for C-ABI functions implemented in Rust.

// NOLINTBEGIN

#ifndef LOG_SURGEON_GENERATED_BINDINGS_HPP
    #define LOG_SURGEON_GENERATED_BINDINGS_HPP

    #include "rust_compat.hpp"

    #include <cstddef>
    #include <cstdint>

namespace log_surgeon {
// https://github.com/mozilla/cbindgen/issues/43
struct Match;
}  // namespace log_surgeon

namespace log_surgeon {
struct Interpretation;

/// Newtype wrapper around a `usize` index.
struct NfaIdx;

/// A parser is almost stateless aside from 2/3 fields:
///
/// - An owned copy of the substring of the input text for the "current" (most recently returned)
/// log event.
///   - This greatly simplifies lifetime management, especially in the presence of FFI,
///     since the parser doesn't own a whole copy of the input text.
///   - Most calls to [`Parser::next_event`] won't require allocation;
///     the memory is bounded by the max log event size.
/// - An owned copy of the text and matches for the header of the next log event.
/// - (Optional/implementation detail) [`TdfaExecution`] cache.
///
struct Parser;

/// A `ParsingSpec` is conceptually a list of rules and a set of delimiter characters.
///
/// [`Rule`]s may be added with a specific integer priority;
/// larger integer value means higher priority.
/// Within a priority level, rules are prioritized by insertion order.
///
struct ParsingSpec;

struct ParsingSpecBuilder;

struct SearchResult;

struct InternalSubQuery;

template <typename T = void>
struct Vec;

/// Index in the parsing specification, offset by/starting at 1.
/// `Option<RuleIdx>` is ABI equivalent to `u16` (for FFI);
/// `None`/`0` represents static text fragments.
using RuleIdx = uint16_t;

/// Can't use `std::range::Range` because it's not `#[repr(C)]`.
template <typename Idx>
struct CRange {
    Idx start;
    Idx end;
};

/// A pointer-length pair with unchecked/untied lifetime.
template <typename T>
struct UncheckedCArray {
    T const* pointer;
    size_t length;

    // Custom
    [[nodiscard]] auto as_cpp_view() const noexcept -> std::string_view
    requires std::is_same_v<T, char>
    {
        return {this->pointer, this->length};
    }
};

struct MatchFfiPointers {
    Match const* parent;
    UncheckedCArray<char> lexeme;
    UncheckedCArray<char> root_rule_name;
    /// Name of _this_ (root or sub-) rule.
    UncheckedCArray<char> rule_name;
    /// Fully-qualified name, including the root rule and all nested regex capture expressions.
    UncheckedCArray<char> fully_qualified_name;
};

/// `Match`es are exposed to FFI, so they need to be `#[repr(C)]`.
struct Match {
    RuleIdx rule_idx;
    /// SubRule ID, local to the containing rule/variable/regex pattern;
    /// `None`/`0` for a root rule,
    /// See [`SubRule`](crate::parsing_spec::SubRule).
    uint16_t sub_rule_id;
    /// Parent SubRule ID, if any;
    /// `None` for both a root rule and a top-level capture in a regex pattern.
    uint16_t parent_id;
    /// Index of the parent in the full list of matches (including variables/root rules).
    /// For a variable, the parent index equals its own index.
    size_t parent_index;
    /// Relative to the start of the log message.
    CRange<size_t> range;
    bool is_leaf;
    /// This should be `Option<EncodingIdx>`,
    /// but cbindgen doesn't translate it to `uint16_t` properly.
    ///
    /// See:
    /// - https://github.com/mozilla/cbindgen/issues/690
    /// - https://github.com/mozilla/cbindgen/issues/326
    /// - https://github.com/mozilla/cbindgen/pull/1029
    uint16_t encoding_idx;
    /// DANGEROUS fields exposed for FFI.
    /// But it's not dangerous if you don't look at it (in Rust).
    /// Safe Rust code should refer to the fields above and the corresponding [`LogEvent`] as
    /// necessary.
    ///
    /// Note: [`LogEvent`] can borrow from [`crate::parser::Parser`] since it's an "external" value,
    /// but the [`Match`]es of a `LogEvent` live in a `Vec` inside `Parser`,
    /// so they can't safely reference the `Parser` (be self-referential).
    MatchFfiPointers ffi_pointers;

    // Custom
    [[nodiscard]] auto get_parent() const noexcept -> Match const* {
        return this->ffi_pointers.parent;
    }

    [[nodiscard]] auto get_lexeme() const noexcept -> std::string_view {
        return this->ffi_pointers.lexeme.as_cpp_view();
    }

    [[nodiscard]] auto get_root_rule_name() const noexcept -> std::string_view {
        return this->ffi_pointers.root_rule_name.as_cpp_view();
    }

    [[nodiscard]] auto get_rule_name() const noexcept -> std::string_view {
        return this->ffi_pointers.rule_name.as_cpp_view();
    }

    [[nodiscard]] auto get_fully_qualified_name() const noexcept -> std::string_view {
        return this->ffi_pointers.fully_qualified_name.as_cpp_view();
    }
};

struct LogEvent {
    /// Strictly speaking, this field is redundant;
    /// however, the spec is needed to get info about the rules,
    /// and is included here for convenience.
    /// Note that since [`Parser::next_event`](crate::parser::Parser::next_event)
    /// returns a `LogEvent` that `mut` (exclusively) borrows from the parser,
    /// the caller can't access the parser's spec and the event at the same time.
    /// So, `Parser::next_event` passes a reference to the spec through the returned `LogEvent`.
    ParsingSpec const* spec;
    CCharArray message;
    /// Matches are sorted:
    ///
    /// 1. left-to-right (lexicographically with respect to the input),
    /// 2. top-down; parent rules first (lexicographically with respect to the regex pattern).
    ///
    /// In particular, root rule matches always come before their sub-rule matches.
    CArray<Match> all_matches;
    CArray<size_t> leaf_indices;
    CArray<size_t> variable_indices;
};

extern "C" {

    /// Enable tracing debugging logs; see [`README.md#Debugging`].
    void log_surgeon_enable_tracing();

    /// Get the matches of a log event.
    Match const* log_surgeon_log_event_all_matches(LogEvent const* log_event, size_t* len);

    Box<LogEvent> log_surgeon_log_event_clone(LogEvent const* value);

    void log_surgeon_log_event_drop(Box<LogEvent> value);

    /// Get the match indices of a log event.
    size_t const* log_surgeon_log_event_leaf_match_indices(LogEvent const* log_event, size_t* len);

    /// Create a (boxed) [`LogEvent`], for cached/reused return value for `log_surgeon_parser_next`.
    Box<LogEvent> log_surgeon_log_event_new();

    Box<Parser> log_surgeon_parser_clone(Parser const* value);

    void log_surgeon_parser_drop(Box<Parser> value);

    /// Consume the (boxed) [`ParsingSpec`] to construct a [`Parser`].
    Box<Parser> log_surgeon_parser_new(Box<ParsingSpec> parsing_spec);

    /// See [`Parser::next_event`].
    bool log_surgeon_parser_next(Parser* parser, CCharArray input, size_t* pos, LogEvent* out);

    /// See [`Parser::reset`].
    void log_surgeon_parser_reset(Parser* parser);

    /// See [`ParsingSpecBuilder::add_encoding`].
    bool log_surgeon_parsing_spec_add_encoding(
            ParsingSpecBuilder* builder,
            CCharArray name,
            CCharArray pattern
    );

    /// See [`ParsingSpecBuilder::add_rule_with_priority`].
    bool log_surgeon_parsing_spec_builder_add_rule_with_priority(
            ParsingSpecBuilder* builder,
            int32_t priority,
            CCharArray name,
            CCharArray pattern
    );

    /// Consume the (boxed) [`ParsingSpecBuilder`] to construct a [`ParsingSpec`].
    Box<ParsingSpec> log_surgeon_parsing_spec_builder_build(Box<ParsingSpecBuilder> builder);

    /// See [`ParsingSpecBuilder::from_parsing_spec_definition`].
    Option<Box<ParsingSpecBuilder>> log_surgeon_parsing_spec_builder_from_definition(
            CCharArray definition
    );

    /// Create a new [`ParsingSpecBuilder`].
    Box<ParsingSpecBuilder> log_surgeon_parsing_spec_builder_new();

    /// See [`ParsingSpecBuilder::set_delimiters`].
    void log_surgeon_parsing_spec_builder_set_delimiters(
            ParsingSpecBuilder* builder,
            CCharArray delimiters
    );

    CCharArray log_surgeon_parsing_spec_get_encoding(Parser const* parser, uint16_t idx);

    Interpretation const*
    log_surgeon_search_get_interpretation(Vec<Interpretation> const* interpretations, size_t i);

    InternalSubQuery const*
    log_surgeon_search_get_sub_query(Interpretation const* interpretation, size_t i);

    void log_surgeon_search_interpretations_drop(Box<Vec<Interpretation>> value);

    Box<Vec<Interpretation>> log_surgeon_search_query_interpretations(
            Parser const* parser,
            CCharArray input,
            CCharArray name
    );

    void log_surgeon_search_result_drop(Box<SearchResult> value);

    Match const*
    log_surgeon_search_result_get_leaf_matches(SearchResult const* search_result, size_t* len);

    CCharArray log_surgeon_search_sub_query_get_qualified_name(InternalSubQuery const* sub_query);

    CCharArray log_surgeon_search_sub_query_get_value(InternalSubQuery const* sub_query);

}  // extern "C"
}  // namespace log_surgeon

#endif  // LOG_SURGEON_GENERATED_BINDINGS_HPP

// NOLINTEND
