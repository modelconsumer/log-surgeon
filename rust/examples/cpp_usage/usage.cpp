#include "log_surgeon/log_surgeon.hpp"

#include <cassert>
#include <cstdio>
#include <iostream>
#include <optional>
#include <span>
#include <string_view>

using namespace log_surgeon;

static void try_interpretations();

int main() {
    ParsingSpecBuilder builder;

    builder.add_rule_with_priority("hello", "abc|d(?<foo>[a-z])f");

    ParserHandle parser{builder.build()};

    CArray<char> const input{"def foobarbaz\n"_rust};
    size_t pos{0};

    std::optional<EventHandle> maybe_event{parser.next_event(input, &pos)};
    assert(maybe_event.has_value());
    assert(pos == input.length);

    EventHandle event{*maybe_event};

    assert(event.get_all_matches().size() == 2);

    {
        Match const& mat{event.get_all_matches()[0]};
        assert(mat.get_rule_name() == "hello");
    }

    {
        std::optional<Match> maybe_match{event.get_leaf_match(0)};
        assert(maybe_match.has_value());

        Match const& mat{*maybe_match};
        assert(mat.get_rule_name() == "foo");
    }

    {
        assert(!event.get_leaf_match(1).has_value());
    }

    try_interpretations();

    printf("good!\n");

    return 0;
}

static void try_interpretations() {
    ParsingSpecBuilder builder;

    builder.add_rule_with_priority("email"_rust, R"((?<user>\w+)@((?<parts>\w+)\.)+(?<tld>\w+))");

    ParserHandle parser{builder.build()};

    std::vector<std::vector<SubQuery>> interpretations{parser.query_interpretations("email"_rust, "a*@*com"_rust)};

    std::cout << "== Interpretations" << std::endl;
    for (std::vector<SubQuery> const& sub_queries : interpretations) {
        std::cout << "- ";
        for (SubQuery const& sub_query : sub_queries) {
            if (sub_query.qualified_name.empty()) {
                std::cout << sub_query.value;
            } else {
                std::cout << "(?<" << sub_query.qualified_name << ">" << sub_query.value << ")";
            }
        }
        std::cout << std::endl;
    }
}
