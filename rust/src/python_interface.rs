use std::num::NonZero;
use std::sync::Arc;

use pyo3::buffer::PyBuffer;
// use pyo3::exceptions::PyIndexError;
// use pyo3::exceptions::PyKeyError;
use pyo3::exceptions::PyRuntimeError;
use pyo3::exceptions::PyUnicodeDecodeError;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use pyo3::types::PyInt;
use pyo3::types::PyList;
use pyo3::types::PyListMethods;
use pyo3::types::PySlice;
use pyo3::types::PyString;

use crate::log_event::LogEvent;
use crate::parser::Parser;
use crate::parsing_spec::ParsingSpec;
use crate::parsing_spec::ParsingSpecBuilder;
use crate::parsing_spec::RootRule;
use crate::parsing_spec::RuleInfo;

pyo3::create_exception!(log_surgeon, LogSurgeonException, PyRuntimeError);

#[pyclass(name = "Parser")]
#[derive(Debug)]
struct PyParser {
	input: Py<PyAny>,
	spec_builder: ParsingSpecBuilder,
	maybe_parser: Option<Parser>,
	buffer: String,
	pos: usize,
	debug: bool,
}

#[pyclass(name = "LogEvent", frozen)]
#[derive(Debug)]
struct PyLogEvent {
	#[pyo3(get)]
	message: Py<PyString>,
	#[pyo3(get)]
	leaf_matches: Py<PyList>,
	#[pyo3(get)]
	non_leaf_matches: Py<PyList>,
	#[pyo3(get)]
	root_matches: Py<PyList>,
	#[pyo3(get)]
	all_matches: Py<PyList>,
}

#[pyclass(name = "Match", frozen)]
#[derive(Debug)]
struct PyMatch {
	#[pyo3(get)]
	root_rule_id: Py<PyInt>,
	/// [`SubRule`](crate::parsing_spec::SubRule) ID; `0` iff this match is a root rule.
	#[pyo3(get)]
	sub_rule_id: Py<PyInt>,

	#[pyo3(get, name = "parent")]
	maybe_parent: Option<Py<PyMatch>>,

	/// Slice indexing into the text of this match.
	#[pyo3(get)]
	offsets: Py<PySlice>,

	/// Non-qualified name of this match.
	#[pyo3(get)]
	name: Py<PyString>,
	/// Fully-qualified name of this match.
	#[pyo3(get)]
	fully_qualified_name: Py<PyString>,

	#[pyo3(get, name = "text")]
	lexeme: Py<PyString>,
}

#[pymethods]
impl PyParser {
	#[new]
	#[pyo3(signature = (*, debug = false))]
	fn new(debug: bool, py: Python<'_>) -> Self {
		Self {
			input: py.None(),
			spec_builder: ParsingSpecBuilder::new(),
			maybe_parser: None,
			buffer: String::new(),
			pos: 0,
			debug,
		}
	}

	/// Raises an exception if `name` is empty, or `"delimiters"`
	/// (see [`ParsingSpecBuilder::add_rule_with_priority`]).
	#[pyo3(signature = (name, pattern, *, priority=0))]
	fn add_rule(&mut self, name: &str, pattern: &str, priority: i32) -> PyResult<()> {
		if name.is_empty() || (name == "delimiters") {
			return Err(PyValueError::new_err(format!("invalid name: '{name:?}'")));
		}
		self.spec_builder
			.add_rule_with_priority(priority, name, pattern)
			.map_err(|err| PyValueError::new_err(format!("invalid pattern: {err:?}")))?;
		Ok(())
	}

	/// Raises an exception if `delimiters` is empty.
	fn set_delimiters(&mut self, delimiters: &str) -> PyResult<()> {
		if delimiters.is_empty() {
			return Err(PyValueError::new_err("delimiters cannot be empty"));
		}
		self.spec_builder.set_delimiters(delimiters);
		Ok(())
	}

	fn compile(&mut self) -> PyResult<()> {
		let spec: ParsingSpec = self.spec_builder.clone().build();
		let spec: Arc<ParsingSpec> = Arc::new(spec);
		self.maybe_parser = Some(Parser::new(spec));
		Ok(())
	}

	fn set_input_stream(&mut self, input: &Bound<'_, PyAny>) -> PyResult<()> {
		self.input = input.clone().unbind();
		self.pos = 0;
		self.buffer.clear();
		read_from_input(input, &mut self.buffer)?;
		Ok(())
	}

	fn next_log_event(&mut self, py: Python<'_>) -> PyResult<Option<PyLogEvent>> {
		if self.done() {
			return Ok(None);
		}

		let Some(parser): Option<&mut Parser> = self.maybe_parser.as_mut() else {
			return Err(LogSurgeonException::new_err("parser has not been compiled"));
		};

		let Some(event): Option<LogEvent<'_>> = parser.next_event(&self.buffer, &mut self.pos) else {
			return Ok(None);
		};

		if self.debug {
			event.check_invariants();
		}

		let leaf_matches: Bound<'_, PyList> = PyList::empty(py);
		let non_leaf_matches: Bound<'_, PyList> = PyList::empty(py);
		let root_matches: Bound<'_, PyList> = PyList::empty(py);
		let mut all_matches: Vec<Bound<'_, PyMatch>> = Vec::new();

		let spec: &ParsingSpec = event.spec;

		for (i, mat) in event.all_matches.iter().enumerate() {
			let rule: &RootRule = &spec[mat.rule_idx];
			let rule_info: &RuleInfo = &rule[mat.sub_rule_id];
			let (name, maybe_parent): (&str, Option<Py<PyMatch>>) = if mat.parent_index < i {
				assert!(!rule_info.is_root());
				(
					rule_info.sub_rule_name(),
					Some(all_matches[mat.parent_index].clone().unbind()),
				)
			} else {
				assert!(rule_info.is_root());
				(&rule.name, None)
			};
			let py_mat: Bound<'_, PyMatch> = PyMatch {
				root_rule_id: PyInt::new(py, u16::from(mat.rule_idx)).unbind(),
				sub_rule_id: PyInt::new(py, mat.sub_rule_id.map_or(0, NonZero::get)).unbind(),
				maybe_parent,
				offsets: PySlice::new(py, mat.range.start as isize, mat.range.end as isize, 1).unbind(),
				name: PyString::new(py, name).unbind(),
				fully_qualified_name: PyString::new(py, &rule_info.fully_qualified_name).unbind(),
				lexeme: PyString::new(py, &event.message[mat.range.start..mat.range.end]).unbind(),
			}
			.into_pyobject(py)?;
			if mat.sub_rule_id.is_none() {
				root_matches.append(py_mat.clone())?;
			}
			if mat.is_leaf {
				leaf_matches.append(py_mat.clone())?;
			} else {
				non_leaf_matches.append(py_mat.clone())?;
			}
			all_matches.push(py_mat);
		}
		let all_matches: Bound<'_, PyList> = PyList::new(py, all_matches)?;

		Ok(Some(PyLogEvent {
			message: PyString::new(py, event.message.as_str()).unbind(),
			leaf_matches: leaf_matches.unbind(),
			non_leaf_matches: non_leaf_matches.unbind(),
			root_matches: root_matches.unbind(),
			all_matches: all_matches.unbind(),
		}))
	}

	fn done(&self) -> bool {
		self.pos == self.buffer.len()
	}

	fn generate_parsing_spec_definition(&self) -> PyResult<String> {
		let Some(parser): Option<&Parser> = self.maybe_parser.as_ref() else {
			return Err(LogSurgeonException::new_err("parser has not been compiled"));
		};

		Ok(parser.spec.to_parsing_spec_definition())
	}

	#[staticmethod]
	#[pyo3(signature = (definition, *, debug = false))]
	fn from_parsing_spec_definition(definition: &str, debug: bool) -> PyResult<Self> {
		match ParsingSpecBuilder::from_parsing_spec_definition(definition) {
			Ok(builder) => {
				let spec: ParsingSpec = builder.build();
				let spec: Arc<ParsingSpec> = Arc::new(spec);
				Ok(Self {
					input: Python::attach(|py| py.None()),
					spec_builder: ParsingSpecBuilder::new(),
					maybe_parser: Some(Parser::new(spec)),
					buffer: String::new(),
					pos: 0,
					debug,
				})
			},
			Err(err) => Err(LogSurgeonException::new_err(format!(
				"invalid parsing spec definition on line {}",
				err.line_offset + 1
			))),
		}
	}
}

#[pymethods]
impl PyLogEvent {
	// #[pyo3(name = "__len__")]
	// fn len(&self) -> usize {
	// 	self.tokens.len()
	// }

	// #[pyo3(name = "__getitem__")]
	// fn get_item(&self, i: usize) -> PyResult<PyToken> {
	// 	if let Some(token) = self.tokens.get(i) {
	// 		Ok(token.clone())
	// 	} else {
	// 		Err(PyIndexError::new_err(format!(
	// 			"event token index {} is out of range 0..{}",
	// 			i,
	// 			self.tokens.len()
	// 		)))
	// 	}
	// }

	#[pyo3(name = "__str__")]
	fn to_string<'py>(this: PyRef<'py, Self>) -> Py<PyString> {
		this.message.clone_ref(this.py())
	}
}

#[pymethods]
impl PyMatch {
	// #[pyo3(name = "__getitem__")]
	// fn get_item(&self, key: &str) -> PyResult<Vec<String>> {
	// 	if let Some(captures) = self.captures.get(key) {
	// 		Ok(captures.clone())
	// 	} else {
	// 		Err(PyKeyError::new_err(format!("token has no capture {}", key)))
	// 	}
	// }

	// #[pyo3(name = "__contains__")]
	// fn contains(&self, key: &str) -> bool {
	// 	self.captures.contains_key(key)
	// }

	#[getter]
	fn root<'py>(this: &Bound<'py, Self>) -> Bound<'py, Self> {
		if let Some(parent) = &this.get().maybe_parent {
			PyMatch::root(parent.bind(this.py()))
		} else {
			this.clone()
		}
	}

	#[pyo3(name = "__repr__")]
	fn repr(&self) -> String {
		format!("{self:?}")
	}
}

/// Returns 0 iff EOF.
fn read_from_input(input: &Bound<'_, PyAny>, output: &mut String) -> PyResult<usize> {
	if let Some(utf8) = python_unicode_or_bytes_as_str(input)? {
		*output += utf8;
		Ok(utf8.len())
	} else {
		let id_read: &Bound<'_, PyString> = pyo3::intern!(input.py(), "read");
		if !input.hasattr(id_read)? {
			return Err(LogSurgeonException::new_err(
				"input stream must be a string, bytes, or a `read`able object",
			));
		}

		// - <https://docs.python.org/3/library/io.html#io.RawIOBase.read>
		//
		// > If `size` is unspecified or -1, all bytes until EOF are read.
		// > If 0 bytes are returned, and size was not 0, this indicates end of file.
		// > If the object is in non-blocking mode and no bytes are available, `None` is returned.
		let data: Bound<'_, PyAny> = input.call_method0(id_read)?;

		if let Some(utf8) = python_unicode_or_bytes_as_str(&data)? {
			*output += utf8;
			Ok(utf8.len())
		} else {
			let buffer: PyBuffer<u8> = PyBuffer::<u8>::get(&data)?;

			let bytes_read: usize = buffer.len_bytes();

			if bytes_read == 0 {
				return Ok(0);
			}

			let mut tmp: Vec<u8> = vec![0; bytes_read];
			buffer.copy_to_slice(input.py(), &mut tmp[..])?;

			*output += str::from_utf8(&tmp[..])?;

			Ok(bytes_read)
		}
	}
}

fn python_unicode_or_bytes_as_str<'a>(input: &'a Bound<'_, PyAny>) -> PyResult<Option<&'a str>> {
	if let Ok(unicode) = input.cast::<PyString>() {
		Ok(Some(unicode.to_str()?))
	} else if let Ok(bytes) = input.cast::<PyBytes>() {
		match str::from_utf8(bytes.as_bytes()) {
			Ok(utf8) => Ok(Some(utf8)),
			Err(err) => Err(PyUnicodeDecodeError::new_err(err)),
		}
	} else {
		Ok(None)
	}
}

#[pymodule]
mod log_surgeon_ffi {
	#[pymodule_export]
	use super::LogSurgeonException;
	#[pymodule_export]
	use super::PyLogEvent;
	#[pymodule_export]
	use super::PyMatch;
	#[pymodule_export]
	use super::PyParser;
	// This looks weird but is correct per PyO3 usage.
	use super::*;

	#[pyfunction]
	fn enable_tracing() {
		crate::enable_tracing();
	}
}
