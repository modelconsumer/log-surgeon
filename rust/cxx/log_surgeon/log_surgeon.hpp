#ifndef LOG_SURGEON_LOG_SURGEON_HPP
#define LOG_SURGEON_LOG_SURGEON_HPP

// IWYU pragma: begin_exports
#include "log_surgeon/generated_bindings.hpp"
#include "log_surgeon/rust_compat.hpp"
// IWYU pragma: end_exports

#include <algorithm>
#include <cassert>
#include <cstddef>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace log_surgeon {
using imp::CRange;
using imp::Interpretation;
using imp::Match;
using imp::ParsingSpec;
using imp::RuleIdx;
using imp::SearchResult;
using imp::UncheckedCArray;

class ParsingSpecBuilder;
class Parser;
class LogEvent;
struct SubQuery;

class ParsingSpecBuilder {
public:
    ParsingSpecBuilder();
    /**
     * Create and initialize a parsing specification builder from a serialized parsing
     * specification.
     *
     * See <https://github.com/y-scope/log-surgeon/blob/log-mechanic/rust/docs/parsing-spec-file.md>
     * for more details.
     * TODO: update link after PR merge.
     *
     * @param definition Definition as a string (file contents, not file path).
     */
    ParsingSpecBuilder(std::string_view definition);

    ~ParsingSpecBuilder();
    ParsingSpecBuilder(ParsingSpecBuilder const& other);
    ParsingSpecBuilder(ParsingSpecBuilder&& other) noexcept;
    auto operator=(ParsingSpecBuilder other) noexcept -> ParsingSpecBuilder&;
    auto operator=(ParsingSpecBuilder&& other) noexcept -> ParsingSpecBuilder&;

    friend auto swap(ParsingSpecBuilder& first, ParsingSpecBuilder& second) noexcept -> void;

    /**
     * Construct the parsing specification and parser from the parsing specification.
     * Afterwards, this builder is in a (defined but) invalid state.
     */
    auto build() -> Parser;

    auto
    add_rule_with_priority(std::string_view name, std::string_view pattern, int32_t priority = 0)
            -> bool;

    auto add_encoding(std::string_view name, std::string_view pattern) -> bool;

private:
    imp::ParsingSpecBuilder* m_builder{};
};

class Parser {
public:
    /**
     * Creates a parser (handle) for the given parsing spec.
     *
     * @param spec An owned`ParsingSpec*` (takes ownership).
     */
    Parser(ParsingSpec* spec);

    ~Parser();
    Parser(Parser const& other);
    Parser(Parser&& other) noexcept;
    auto operator=(Parser other) noexcept -> Parser&;
    auto operator=(Parser&& other) noexcept -> Parser&;

    /**
     * Conventional `swap` function, declared using `friend` for ADL.
     * Also the second critical piece for the copy-and-swap idiom.
     *
     * @param first
     * @param second
     */
    friend auto swap(Parser& first, Parser& second) noexcept -> void;

    /**
     * Get the next log event, as a handle.
     * Conceptually, `pos` is the current position/offset to parse the next event;
     * specifically, it's passed as a pointer so log surgeon advances it before returning.
     *
     * @param input A view of the entire input text.
     * @param pos Pointer to an offset value in the text.
     * @return `std::nullopt` iff EOF.
     */
    [[nodiscard]] auto next_event(std::string_view input, size_t* pos) -> std::optional<LogEvent>;

    /**
     * Reset the internal state of the parser;
     * e.g. when refilling a partial buffer and re-parsing the last log event.
     */
    auto reset();

    /**
     * Computes interpretations for a query.
     *
     * @param name
     * @param query
     */
    [[nodiscard]] auto query_interpretations(std::string_view name, std::string_view query)
            -> std::vector<std::vector<SubQuery>>;

private:
    /**
     * Last piece of copy-and-swap;
     * private since we only want this for copy-and-swap.
     */
    Parser() noexcept = default;

    imp::Parser* m_parser{};
    imp::LogEvent* m_event{};
};

class LogEvent {
public:
    /**
     * @param event A borrowed `imp::LogEvent const*` (doesn't take ownership).
     * @param parser A borrowed `imp::Parser const*` (doesn't take ownership).
     */
    LogEvent(imp::LogEvent const* event);

    [[nodiscard]] auto get_all_matches() const -> std::span<Match const> { return m_matches; }

    /**
     * Used to iterate over leaf matches of a log event;
     * done when this function returns `std::nullopt`.
     *
     * @param i Try to get the `i`th match.
     * @return `std::nullopt` iff out of range.
     */
    [[nodiscard]] auto get_leaf_match(size_t i) const -> std::optional<Match>;

    [[nodiscard]] auto get_message() const -> std::string_view;

private:
    imp::LogEvent const* m_event;
    std::span<Match const> m_matches;
    std::span<size_t const> m_leaf_indices;
};

struct SubQuery {
    std::string qualified_name;
    std::string value;
};

inline ParsingSpecBuilder::ParsingSpecBuilder()
        : m_builder{imp::log_surgeon_parsing_spec_builder_new()} {}

inline ParsingSpecBuilder::ParsingSpecBuilder(std::string_view definition)
        : m_builder{imp::log_surgeon_parsing_spec_builder_from_definition(
                  CCharArray::from_string_view(definition)
          )} {}

inline ParsingSpecBuilder::~ParsingSpecBuilder() {
    if (nullptr != m_builder) {
        imp::log_surgeon_parsing_spec_builder_drop(m_builder);
    }
}

inline ParsingSpecBuilder::ParsingSpecBuilder(ParsingSpecBuilder const& other)
        : ParsingSpecBuilder{} {
    // Copy-and swap idiom: The first "centerpiece";
    // the "semantics" of this type's resource management must be
    // bona fide implemented here.
    m_builder = imp::log_surgeon_parsing_spec_builder_clone(other.m_builder);
}

inline ParsingSpecBuilder::ParsingSpecBuilder(ParsingSpecBuilder&& other) noexcept
        : ParsingSpecBuilder{} {
    // Copy-and-swap idiom: The move constructor is handled by the same
    // `swap` mechanism used to safely implement copy assignment.
    swap(*this, other);
}

inline auto ParsingSpecBuilder::operator=(ParsingSpecBuilder other) noexcept
        -> ParsingSpecBuilder& {
    // Copy-and-swap idiom: It is important that `other` is taken by value.
    // This would handle both copy and move assignment;
    // when called with an rvalue reference,
    // the compiler would use the move constructor to create `other`,
    // which we then swap with.
    // Supposedly, that allows for better optimization opportunities too.
    swap(*this, other);
    return *this;
}

inline auto ParsingSpecBuilder::operator=(ParsingSpecBuilder&& other) noexcept
        -> ParsingSpecBuilder& {
    // Copy-and-swap idiom: Duplicate of copy assignment;
    // lints aren't smart enough to realize that this would be covered as above.
    swap(*this, other);
    return *this;
}

inline auto swap(ParsingSpecBuilder& first, ParsingSpecBuilder& second) noexcept -> void {
    using std::swap;

    swap(first.m_builder, second.m_builder);
}

inline auto ParsingSpecBuilder::build() -> Parser {
    if (nullptr == m_builder) {
        throw std::invalid_argument("builder already constructed");
    }
    Parser parser{imp::log_surgeon_parsing_spec_builder_build(m_builder)};
    m_builder = nullptr;
    return parser;
}

inline auto ParsingSpecBuilder::add_rule_with_priority(
        std::string_view name,
        std::string_view pattern,
        int32_t priority
) -> bool {
    if (nullptr == m_builder) {
        throw std::invalid_argument("builder already constructed");
    }
    return imp::log_surgeon_parsing_spec_builder_add_rule_with_priority(
            m_builder,
            priority,
            CCharArray::from_string_view(name),
            CCharArray::from_string_view(pattern)
    );
}

inline auto ParsingSpecBuilder::add_encoding(std::string_view name, std::string_view pattern)
        -> bool {
    if (nullptr == m_builder) {
        throw std::invalid_argument("builder already constructed");
    }
    return imp::log_surgeon_parsing_spec_builder_add_encoding(
            m_builder,
            CCharArray::from_string_view(name),
            CCharArray::from_string_view(pattern)
    );
}

inline Parser::Parser(ParsingSpec* spec) : Parser{} {
    if (nullptr == spec) {
        throw std::invalid_argument("spec must not be null");
    }
    m_parser = imp::log_surgeon_parser_new(spec);
    m_event = imp::log_surgeon_log_event_new();
}

inline Parser::~Parser() {
    if (nullptr != m_event) {
        imp::log_surgeon_log_event_drop(m_event);
    }
    if (nullptr != m_parser) {
        imp::log_surgeon_parser_drop(m_parser);
    }
}

inline Parser::Parser(Parser const& other) : Parser{} {
    // Copy-and swap idiom: The first "centerpiece";
    // the "semantics" of this type's resource management must be
    // bona fide implemented here.
    m_parser = imp::log_surgeon_parser_clone(other.m_parser);
    m_event = imp::log_surgeon_log_event_clone(other.m_event);
}

inline Parser::Parser(Parser&& other) noexcept : Parser{} {
    // Copy-and-swap idiom: The move constructor is handled by the same
    // `swap` mechanism used to safely implement copy assignment.
    swap(*this, other);
}

inline auto Parser::operator=(Parser other) noexcept -> Parser& {
    // Copy-and-swap idiom: It is important that `other` is taken by value.
    // This would handle both copy and move assignment;
    // when called with an rvalue reference,
    // the compiler would use the move constructor to create `other`,
    // which we then swap with.
    // Supposedly, that allows for better optimization opportunities too.
    swap(*this, other);
    return *this;
}

inline auto Parser::operator=(Parser&& other) noexcept -> Parser& {
    // Copy-and-swap idiom: Duplicate of copy assignment;
    // lints aren't smart enough to realize that this would be covered as above.
    swap(*this, other);
    return *this;
}

inline auto swap(Parser& first, Parser& second) noexcept -> void {
    using std::swap;

    swap(first.m_parser, second.m_parser);
    swap(first.m_event, second.m_event);
}

inline auto Parser::next_event(std::string_view input, size_t* pos) -> std::optional<LogEvent> {
    if (!log_surgeon_parser_next(m_parser, CCharArray::from_string_view(input), pos, m_event)) {
        return std::nullopt;
    }
    return std::make_optional(LogEvent{m_event});
}

inline auto Parser::reset() {
    imp::log_surgeon_parser_reset(m_parser);
}

inline auto Parser::query_interpretations(std::string_view name, std::string_view query)
        -> std::vector<std::vector<SubQuery>> {
    std::vector<std::vector<SubQuery>> interpretations;

    Box<Vec<Interpretation>> rust_interpretations{imp::log_surgeon_search_query_interpretations(
            m_parser,
            CCharArray::from_string_view(query),
            CCharArray::from_string_view(name)
    )};

    size_t i{0};
    while (true) {
        Interpretation const* interpretation{
                imp::log_surgeon_search_get_interpretation(rust_interpretations, i)
        };
        if (nullptr == interpretation) {
            break;
        }

        std::vector<SubQuery> sub_queries;
        size_t j{0};
        while (true) {
            imp::SubQuery const* sub_query{log_surgeon_search_get_sub_query(interpretation, j)};
            if (nullptr == sub_query) {
                break;
            }

            std::string_view const qualified_name{
                    imp::log_surgeon_search_sub_query_get_qualified_name(sub_query)
            };
            std::string_view const value{imp::log_surgeon_search_sub_query_get_value(sub_query)};

            sub_queries.push_back({
                    .qualified_name = std::string{qualified_name},
                    .value = std::string{value},
            });

            j++;
        }
        interpretations.push_back(std::move(sub_queries));

        i++;
    }

    imp::log_surgeon_search_interpretations_drop(rust_interpretations);

    return interpretations;
}

inline LogEvent::LogEvent(imp::LogEvent const* event) : m_event(event) {
    m_matches = event->all_matches.as_span();
    m_leaf_indices = event->leaf_indices.as_span();
}

inline auto LogEvent::get_leaf_match(size_t i) const -> std::optional<Match> {
    if (i < m_leaf_indices.size()) {
        // `std::span` doesn't have `.at()` until C++26...
        // NOLINTNEXTLINE(cppcoreguidelines-pro-bounds-avoid-unchecked-container-access)
        return std::make_optional(m_matches[m_leaf_indices[i]]);
    }
    return std::nullopt;
}

inline auto LogEvent::get_message() const -> std::string_view {
    return m_event->message;
}
}  // namespace log_surgeon

#endif  // LOG_SURGEON_LOG_SURGEON_HPP
