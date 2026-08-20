mod encoding;
mod rule;
mod spec_file;

use std::collections::BTreeMap;
use std::num::NonZero;
use std::sync::Arc;

pub use encoding::Encoding;
pub use encoding::EncodingIdx;
pub use rule::RootRule;
pub use rule::RuleIdx;
pub use rule::RuleInfo;
pub use rule::SubRule;

use crate::dfa::CompressedDfa;
use crate::dfa::Tdfa;
use crate::nfa::Tnfa;
use crate::regex::AnchoredRegex;
use crate::regex::Regex;
use crate::regex::RegexPlaceholderLookup;

#[derive(Debug, Clone)]
pub struct ParsingSpecBuilder {
	rules_by_priority: BTreeMap<i32, Vec<(Arc<str>, AnchoredRegex)>>,
	placeholders: BTreeMap<String, Regex>,
	encodings: Vec<Arc<Encoding>>,

	/// Cached (canonicalized) DFA for [`ParsingSpec::main_dfa`].
	maybe_cached_dfa: Option<Tdfa>,

	delimiters: String,
}

/// A `ParsingSpec` is conceptually a list of rules and a set of delimiter characters.
///
/// [`Rule`]s may be added with a specific integer priority;
/// larger integer value means higher priority.
/// Within a priority level, rules are prioritized by insertion order.
///
#[derive(Debug, Clone)]
pub struct ParsingSpec {
	pub rules: Vec<RootRule>,
	pub placeholders: BTreeMap<String, Regex>,

	pub delimiters: String,

	pub encodings: Vec<Arc<Encoding>>,

	/// DFA used for lexing/parsing;
	/// determine which root rule matched, without tags for matching sub-rules.
	pub main_dfa: Tdfa,
	/// TNFA used for search.
	pub main_nfa: Tnfa,
	/// TODO
	pub optimized_dfa: CompressedDfa,

	/// Derived from `delimiters`.
	pub ascii_delimiters: [bool; 0x80],
	pub non_ascii_delimiters: String,
}

impl Eq for ParsingSpec {}

impl PartialEq for ParsingSpec {
	fn eq(&self, other: &Self) -> bool {
		(&self.rules, &self.delimiters, &self.placeholders, &self.encodings).eq(&(
			&other.rules,
			&other.delimiters,
			&other.placeholders,
			&other.encodings,
		))
	}
}

impl ParsingSpecBuilder {
	pub fn new() -> Self {
		Self {
			rules_by_priority: BTreeMap::new(),
			placeholders: BTreeMap::new(),
			encodings: Vec::new(),
			maybe_cached_dfa: None,
			delimiters: ParsingSpec::DEFAULT_DELIMITERS.to_owned(),
		}
	}

	/// Panics if `delimiters` is empty.
	pub fn set_delimiters<LikeString>(&mut self, delimiters: LikeString) -> &mut Self
	where
		LikeString: Into<String>,
	{
		let mut delimiters: String = delimiters.into();
		assert!(!delimiters.is_empty());
		if !delimiters.contains('\n') {
			delimiters.push('\n');
		}
		self.delimiters = delimiters;
		self
	}

	/// Adds a rule with the default priority `0`.
	///
	/// Panics if `name` is empty or one of the reserved words:
	///
	/// - `"delimiters"`
	///
	pub fn add_rule<LikeString, RegexOrPattern>(
		&mut self,
		name: LikeString,
		regex: RegexOrPattern,
	) -> Result<&mut Self, RegexOrPattern::Error>
	where
		LikeString: Into<Arc<str>>,
		RegexOrPattern: TryInto<AnchoredRegex>,
	{
		self.add_rule_with_priority(0, name, regex)
	}

	/// Adds a rule with the given priority; larger integer value has higher priority.
	/// Within a priority level, rules are prioritized by insertion order.
	///
	/// Panics if `name` is empty or one of the reserved words:
	///
	/// - `"delimiters"`
	///
	pub fn add_rule_with_priority<LikeString, RegexOrPattern>(
		&mut self,
		priority: i32,
		name: LikeString,
		regex: RegexOrPattern,
	) -> Result<&mut Self, RegexOrPattern::Error>
	where
		LikeString: Into<Arc<str>>,
		RegexOrPattern: TryInto<AnchoredRegex>,
	{
		let name: Arc<str> = name.into();
		assert!(!name.is_empty());
		assert_ne!(&*name, "delimiters");

		let regex: AnchoredRegex = regex.try_into()?;

		let rules: &mut Vec<(Arc<str>, AnchoredRegex)> =
			self.rules_by_priority.entry(priority).or_insert_with(Vec::new);

		rules.push((name, regex));

		Ok(self)
	}

	/// Panics if `name` is empty or one of the reserved words:
	///
	/// - `"delimiters"`
	///
	pub fn add_placeholder<LikeString>(&mut self, name: LikeString, regex: Regex) -> Result<&mut Self, Regex>
	where
		LikeString: Into<String>,
	{
		let name: String = name.into();
		assert!(!name.is_empty());
		assert_ne!(name, "delimiters");

		let maybe_old: Option<Regex> = self.placeholders.insert(name, regex);
		if let Some(old) = maybe_old {
			return Err(old);
		}

		Ok(self)
	}

	/// Panics if `name` is empty.
	pub fn add_encoding<LikeString>(&mut self, name: LikeString, regex: Regex) -> Result<&mut Self, Arc<Encoding>>
	where
		LikeString: Into<String>,
	{
		let name: String = name.into();
		assert!(!name.is_empty());

		if let Some(other) = self.encodings.iter().find(|other| other.name == name) {
			return Err(other.clone());
		}

		let idx: NonZero<u16> = u16::try_from(self.encodings.len())
			.ok()
			.and_then(|n| NonZero::<u16>::MIN.checked_add(n))
			.expect("too many encodings");
		let idx: EncodingIdx = EncodingIdx::from(idx);
		let nfa: Tnfa = Tnfa::for_regex(&regex);
		self.encodings.push(Arc::new(Encoding { idx, name, regex, nfa }));

		Ok(self)
	}

	pub fn set_cached_dfa(&mut self, mut cached: Tdfa) -> &mut Self {
		cached.initialize_ascii_cache();
		self.maybe_cached_dfa = Some(cached);
		self
	}

	pub fn build(self) -> ParsingSpec {
		let mut rules: Vec<RootRule> = Vec::new();

		let mut index: NonZero<u16> = NonZero::<u16>::MIN;
		for (priority, rules_at_priority) in self.rules_by_priority.into_iter().rev() {
			for (rule_name, rule_regex) in rules_at_priority.into_iter() {
				if rule_regex.total_captures == NonZero::<u16>::MIN {
					let leaf_nfa: Tnfa = Tnfa::for_regex(&rule_regex.regex);

					for enc in self.encodings.iter() {
						let intersection: Tnfa = leaf_nfa.intersect::<false>(&enc.nfa);

						if !intersection.can_accept() {
							continue;
						}

						let rule_idx: RuleIdx = RuleIdx::new(index);

						let dfa: Tdfa = Tdfa::determinization(&intersection);

						rules.push(RootRule::new(
							rule_idx,
							rule_name.clone(),
							priority,
							rule_regex.clone(),
							Some(enc.clone()),
							dfa,
						));

						index = index
							.checked_add(1)
							.expect("more than `u16::MAX` rules (not supported)");
					}
				}

				let rule_idx: RuleIdx = RuleIdx::new(index);

				let dfa: Tdfa = Tdfa::for_single_rule(rule_idx, &rule_regex.regex, &self.encodings);

				rules.push(RootRule::new(rule_idx, rule_name, priority, rule_regex, None, dfa));

				index = index
					.checked_add(1)
					.expect("more than `u16::MAX` rules (not supported)");
			}
		}

		let main_nfa: Tnfa = Tnfa::for_rules::<true, _>(rules.iter(), &self.delimiters);

		let main_dfa: Tdfa = self.maybe_cached_dfa.unwrap_or_else(|| {
			now!(t0);
			let main_dfa: Tdfa = Tdfa::for_rules(rules.iter(), self.delimiters.clone());
			now!(t1);
			let minimized: Tdfa = main_dfa.canonicalize();
			now!(t2);
			debug!(
				"[minimizing dfa] took ({:?}, {:?})",
				t1.duration_since(t0),
				t2.duration_since(t1)
			);
			minimized
		});

		let optimized_dfa: CompressedDfa = main_dfa.compress();

		let mut ascii_delimiters: [bool; 0x80] = [false; 0x80];
		let mut non_ascii_delimiters: String = String::new();
		for ch in self.delimiters.chars() {
			if let Ok(i) = u8::try_from(ch)
				&& let Some(entry) = ascii_delimiters.get_mut(usize::from(i))
			{
				*entry = true;
			} else {
				non_ascii_delimiters.push(ch);
			}
		}

		ParsingSpec {
			rules,
			placeholders: self.placeholders,
			delimiters: self.delimiters,
			encodings: self.encodings,
			main_nfa,
			main_dfa,
			optimized_dfa,
			ascii_delimiters,
			non_ascii_delimiters,
		}
	}
}

impl RegexPlaceholderLookup for ParsingSpecBuilder {
	fn lookup(&mut self, name: &str) -> Option<Regex> {
		self.placeholders.get(name).cloned()
	}
}

impl ParsingSpec {
	pub const DEFAULT_DELIMITERS: &str = " \t\r\n:,!;%";

	pub const BLANK: Self = Self {
		rules: Vec::new(),
		placeholders: BTreeMap::new(),
		delimiters: String::new(),
		encodings: Vec::new(),
		main_dfa: Tdfa::BLANK,
		main_nfa: Tnfa::BLANK,
		optimized_dfa: CompressedDfa::BLANK,
		ascii_delimiters: [false; 0x80],
		non_ascii_delimiters: String::new(),
	};

	/// (Sub)rules with the exact fully qualified name match.
	pub fn rules_for_name(&self, name: &str) -> Vec<(&RuleInfo, &Regex)> {
		let parts: Vec<&str> = name.split('.').collect::<Vec<_>>();
		let rule_name: &str = parts.first().copied().expect("name fragment is empty");
		let capture_names: &[&str] = &parts[1..];

		if let Some(first) = capture_names.first().copied() {
			let mut possibilities: Vec<(&RuleInfo, &Regex)> = Vec::new();
			for root_rule in self.rules.iter() {
				if &*root_rule.name != rule_name {
					continue;
				}
				root_rule.find_capture(&root_rule.regex.regex, first, &capture_names[1..], &mut possibilities);
			}
			possibilities
		} else {
			// Just a root name; no trailing parts.
			self.rules
				.iter()
				.filter(|root_rule| &*root_rule.name == rule_name)
				.map(|root_rule| (&root_rule[None], &root_rule.regex.regex))
				.collect::<Vec<_>>()
		}
	}

	/// Converts a shape string to a regular expression.
	/// References to rules (by name) should be enclosed with percent symbols as `%foo.bar%`.
	/// Returns `Err(name)` if a name is not found.
	pub fn shape_as_automata(&self, shape: &str) -> Result<Tnfa, String> {
		enum Kind {
			Text(String),
			Rule(String),
		}

		let mut sequence: Vec<Tnfa> = Vec::new();

		let mut current: Kind = Kind::Text(String::new());

		for ch in shape.chars() {
			match current {
				Kind::Text(mut buffer) => {
					if ch == '%' {
						// Append static text
						let regex: Regex = Regex::Sequence(buffer.chars().map(Regex::Literal).collect::<Vec<_>>());
						sequence.push(Tnfa::for_regex(&regex));

						// Switch to parsing rule name.
						current = Kind::Rule(String::new());
					} else {
						buffer.push(ch);
						current = Kind::Text(buffer);
					}
				},
				Kind::Rule(mut rule_name) => {
					if ch == '%' {
						// Append rule regexes.
						let rules: Vec<(&RuleInfo, &Regex)> = self.rules_for_name(&rule_name);
						if rules.is_empty() {
							return Err(rule_name);
						}
						let branches: Tnfa = rules
							.iter()
							.map(|&(info, regex)| {
								let regex: &Regex = if info.is_root() && !self[info.root_idx].has_captures() {
									&Regex::Capture(
										Arc::new(SubRule {
											name: info.root_name.clone(),
											regex: regex.clone(),
											id: NonZero::<u16>::MAX,
											parent_id: None,
											descendents: 0,
											qualified_name: info.root_name.clone(),
											fully_qualified_name: info.root_name.clone(),
										})
										.into(),
									)
								} else {
									regex
								};
								Tnfa::for_single_rule(info.root_idx, regex, &[])
							})
							.fold(Tnfa::BLANK, |accum, x| accum.or(&x));
						sequence.push(branches);

						// Switch to static text.
						current = Kind::Text(String::new());
					} else {
						rule_name.push(ch);
						current = Kind::Rule(rule_name);
					}
				},
			}
		}

		let Kind::Text(buffer): Kind = current else {
			panic!("malformed log shape '{}'", shape.escape_default());
		};

		let regex: Regex = Regex::Sequence(buffer.chars().map(Regex::Literal).collect::<Vec<_>>());
		sequence.push(Tnfa::for_regex(&regex));

		Ok(sequence.into_iter().fold(Tnfa::BLANK, |accum, x| accum.concat(&x)))
	}
}

impl std::ops::Index<RuleIdx> for ParsingSpec {
	type Output = RootRule;

	fn index(&self, idx: RuleIdx) -> &Self::Output {
		&self.rules[usize::from(u16::from(idx)) - 1]
	}
}

impl std::ops::Index<EncodingIdx> for ParsingSpec {
	type Output = Arc<Encoding>;

	fn index(&self, idx: EncodingIdx) -> &Self::Output {
		&self.encodings[usize::from(u16::from(idx) - 1)]
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::log_event::LogEvent;
	use crate::parser::Parser;

	#[test]
	fn number_encoding() {
		let mut builder: ParsingSpecBuilder = ParsingSpecBuilder::new();
		builder
			.add_rule("has_number", r"\w*\d\w*")
			.unwrap()
			.add_rule("ip_address", r"(?<first>\d+)(\.\d+){3}")
			.unwrap()
			.add_encoding("int", Regex::from_pattern(r"\d+").unwrap())
			.unwrap();

		let spec: ParsingSpec = builder.build();
		assert_eq!(spec.rules.len(), 3);

		let mut parser: Parser = Parser::new(Arc::new(spec));

		let event: LogEvent<'_> = parser.next_event("a1b", &mut 0).unwrap();
		assert_eq!(event.all_matches.len(), 1);
		assert_eq!(event.all_matches[0].rule_idx, RuleIdx::from(NonZero::new(2).unwrap()));
		assert_eq!(event.all_matches[0].encoding_idx, None);

		let event: LogEvent<'_> = parser.next_event("123", &mut 0).unwrap();
		assert_eq!(event.all_matches.len(), 1);
		assert_eq!(event.all_matches[0].rule_idx, RuleIdx::from(NonZero::new(1).unwrap()));
		assert_eq!(event.all_matches[0].encoding_idx.unwrap(), NonZero::new(1).unwrap());

		let event: LogEvent<'_> = parser.next_event("12.34.56.78", &mut 0).unwrap();
		assert_eq!(event.all_matches.len(), 2);
		assert_eq!(event.all_matches[0].rule_idx, RuleIdx::from(NonZero::new(3).unwrap()));
		assert_eq!(event.all_matches[0].encoding_idx, None);
		assert_eq!(event.all_matches[1].encoding_idx.unwrap(), NonZero::new(1).unwrap());
	}
}
