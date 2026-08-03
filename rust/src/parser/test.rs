use super::*;

#[test]
fn ip_address() {
	let mut parser: Parser = parser! {
		r#"
		ip_address: "\d+\.\d+\.\d+\.\d+"
		"#
	};

	let input: &str = "hello 1.2.3.4";
	let mut pos: usize = 0;

	{
		let event: LogEvent<'_> = parser.next_event(input, &mut pos).unwrap();
		assert_eq!(&*event.message, "hello 1.2.3.4");
		assert_eq!(event.all_matches.len(), 1);
		assert_eq!(event.all_matches[0].range, CRange::new("hello ".len(), input.len()));
	}

	assert_eq!(pos, input.len());
}
