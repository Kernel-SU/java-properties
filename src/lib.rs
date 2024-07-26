// -*-  indent-tabs-mode:nil; tab-width:4;  -*-
//! Utilities for reading and writing Java properties files
//!
//! The specification is taken from <https://docs.oracle.com/javase/7/docs/api/java/util/Properties.html>.
//! Where the documentation is ambiguous or incomplete, behavior is based on the behavior of java.util.Properties.
//!
//! # Examples
//!
//! ```
//! use java_properties::PropertiesIter;
//! use java_properties::PropertiesWriter;
//! use java_properties::read;
//! use java_properties::write;
//! use std::collections::HashMap;
//! use std::env::temp_dir;
//! use std::fs::File;
//! use std::io::BufReader;
//! use std::io::BufWriter;
//! use std::io::prelude::*;
//!
//! # fn main() -> std::result::Result<(), java_properties::PropertiesError> {
//! let mut file_name = temp_dir();
//! file_name.push("java-properties-test.properties");
//!
//! // Writing simple
//! let mut src_map1 = HashMap::new();
//! src_map1.insert("a".to_string(), "b".to_string());
//! let mut f = File::create(&file_name)?;
//! write(BufWriter::new(f), &src_map1)?;
//!
//! // Writing advanced
//! let mut src_map2 = HashMap::new();
//! src_map2.insert("a".to_string(), "b".to_string());
//! let mut f = File::create(&file_name)?;
//! let mut writer = PropertiesWriter::new(BufWriter::new(f));
//! for (k, v) in &src_map2 {
//!   writer.write(&k, &v)?;
//! }
//! writer.finish();
//!
//! // Reading simple
//! let mut f2 = File::open(&file_name)?;
//! let dst_map1 = read(BufReader::new(f2))?;
//! assert_eq!(src_map1, dst_map1);
//!
//! // Reading advanced
//! let mut f = File::open(&file_name)?;
//! let mut dst_map2 = HashMap::new();
//! PropertiesIter::new(BufReader::new(f)).read_into(|k, v| {
//!   dst_map2.insert(k, v);
//! })?;
//! assert_eq!(src_map2, dst_map2);
//! # Ok(())
//! # }
//! ```

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::invalid_codeblock_attributes)]
#![deny(rustdoc::invalid_rust_codeblocks)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(unused_must_use)]
#![doc(test(attr(allow(unused_extern_crates))))]
#![doc(test(attr(deny(unused_must_use))))]
#![doc(test(attr(warn(unused))))]
#![warn(missing_docs)]

use encoding_rs::CoderResult;
use encoding_rs::Decoder;
use encoding_rs::Encoder;
use encoding_rs::EncoderResult;
use encoding_rs::Encoding;
use encoding_rs::WINDOWS_1252;
use lazy_static::lazy_static;
#[cfg(feature = "unicode")]
use regex::Regex;
#[cfg(not(feature = "unicode"))]
use regex_lite::Regex;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::convert::From;
use std::error::Error;
use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::io;
use std::io::Read;
use std::io::Write;
use std::iter::Peekable;
use std::ops::Deref;

/////////////////////

/// The error type for reading and writing properties files.
#[derive(Debug)]
pub struct PropertiesError {
    description: String,
    cause: Option<Box<dyn Error + 'static + Send + Sync>>,
    line_number: Option<usize>,
}

impl PropertiesError {
    fn new<S: Into<String>>(
        description: S,
        cause: Option<Box<dyn Error + 'static + Send + Sync>>,
        line_number: Option<usize>,
    ) -> Self {
        PropertiesError {
            description: description.into(),
            cause,
            line_number,
        }
    }

    /// Returns the 1-based line number associated with the error, if available.
    pub fn line_number(&self) -> Option<usize> {
        self.line_number
    }
}

impl Error for PropertiesError {
    fn description(&self) -> &str {
        &self.description
    }

    // The "readable" version is less readable, especially since it requires manual type assertions.
    #[allow(clippy::manual_map)]
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self.cause {
            Some(ref c) => Some(c.deref()),
            None => None,
        }
    }
}

impl From<io::Error> for PropertiesError {
    fn from(e: io::Error) -> Self {
        PropertiesError::new("I/O error", Some(Box::new(e)), None)
    }
}

impl Display for PropertiesError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.description)?;
        match self.line_number {
            Some(n) => write!(f, " (line_number = {})", n),
            None => write!(f, " (line_number = unknown)"),
        }
    }
}

/////////////////////

struct DecodeIter<R: Read> {
    decoder: Decoder,
    reader: R,
    input_buffer: Vec<u8>,
    output_buffer: String,
    chars: VecDeque<char>,
}

impl<R: Read> DecodeIter<R> {
    fn new(reader: R, encoding: &'static Encoding) -> Self {
        Self {
            decoder: encoding.new_decoder(),
            reader,
            // must have a non-zero capacity since we double it as needed
            input_buffer: Vec::with_capacity(64),
            // must have a non-zero capacity since we double it as needed
            output_buffer: String::with_capacity(64),
            chars: VecDeque::new(),
        }
    }
}

impl<R: Read> Iterator for DecodeIter<R> {
    type Item = Result<char, io::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(c) = self.chars.pop_front() {
                return Some(Ok(c));
            }
            let reader_eof = if self.input_buffer.is_empty() {
                self.input_buffer.resize(self.input_buffer.capacity(), 0);
                let bytes_read = match self.reader.read(&mut self.input_buffer) {
                    Ok(x) => x,
                    Err(e) => {
                        self.input_buffer.clear();
                        return Some(Err(e));
                    }
                };
                self.input_buffer.truncate(bytes_read);
                bytes_read == 0
            } else {
                false
            };
            let (result, bytes_read, _) = self.decoder.decode_to_string(
                &self.input_buffer,
                &mut self.output_buffer,
                reader_eof,
            );
            self.input_buffer.drain(..bytes_read);
            match result {
                CoderResult::InputEmpty => (),
                CoderResult::OutputFull => {
                    self.output_buffer.reserve(self.output_buffer.capacity());
                }
            };
            self.chars.extend(self.output_buffer.drain(..));
            if self.chars.is_empty() && reader_eof {
                return None;
            }
        }
    }
}

/////////////////////

#[derive(PartialEq, Eq, Debug)]
struct NaturalLine(usize, String);

// We can't use BufRead.lines() because it doesn't use the proper line endings
struct NaturalLines<R: Read> {
    chars: Peekable<DecodeIter<R>>,
    eof: bool,
    line_count: usize,
}

impl<R: Read> NaturalLines<R> {
    fn new(reader: R, encoding: &'static Encoding) -> Self {
        NaturalLines {
            chars: DecodeIter::new(reader, encoding).peekable(),
            eof: false,
            line_count: 0,
        }
    }
}

const LF: char = '\n';
const CR: char = '\r';

impl<R: Read> Iterator for NaturalLines<R> {
    type Item = Result<NaturalLine, PropertiesError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }
        let mut buf = String::new();
        loop {
            match self.chars.next() {
                Some(Ok(CR)) => {
                    if let Some(&Ok(LF)) = self.chars.peek() {
                        self.chars.next();
                    }
                    self.line_count += 1;
                    return Some(Ok(NaturalLine(self.line_count, buf)));
                }
                Some(Ok(LF)) => {
                    self.line_count += 1;
                    return Some(Ok(NaturalLine(self.line_count, buf)));
                }
                Some(Ok(c)) => buf.push(c),
                Some(Err(e)) => {
                    return Some(Err(PropertiesError::new(
                        "I/O error",
                        Some(Box::new(e)),
                        Some(self.line_count + 1),
                    )))
                }
                None => {
                    self.eof = true;
                    self.line_count += 1;
                    return Some(Ok(NaturalLine(self.line_count, buf)));
                }
            }
        }
    }
}

/////////////////////

#[derive(PartialEq, Eq, Debug)]
struct LogicalLine(usize, String);

struct LogicalLines<I: Iterator<Item = Result<NaturalLine, PropertiesError>>> {
    physical_lines: I,
    eof: bool,
}

impl<I: Iterator<Item = Result<NaturalLine, PropertiesError>>> LogicalLines<I> {
    fn new(physical_lines: I) -> Self {
        LogicalLines {
            physical_lines,
            eof: false,
        }
    }
}

fn count_ending_backslashes(s: &str) -> usize {
    let mut n = 0;
    for c in s.chars() {
        if c == '\\' {
            n += 1;
        } else {
            n = 0;
        }
    }
    n
}

impl<I: Iterator<Item = Result<NaturalLine, PropertiesError>>> Iterator for LogicalLines<I> {
    type Item = Result<LogicalLine, PropertiesError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eof {
            return None;
        }
        let mut buf = String::new();
        let mut first = true;
        let mut line_number = 0;
        loop {
            match self.physical_lines.next() {
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(NaturalLine(line_no, line))) => {
                    if first {
                        line_number = line_no;
                    }
                    buf.push_str(if first { &line } else { line.trim_start() });
                    lazy_static! {
                        static ref COMMENT_RE: Regex = Regex::new("^[ \t\r\n\x0c]*[#!]").unwrap();
                    }
                    if first && COMMENT_RE.is_match(&line) {
                        // This format is terrible.  We can't throw out comment lines before joining natural lines, because "a\\\n#b" should be joined into "a#b".
                        // On the other hand, we can't join natural lines before processing comments, because "#a\\\nb" should stay as two lines, "#a\\" and "b".
                        // Processing line joins and comments are inextricably linked.
                        assert!(line_number != 0);
                        return Some(Ok(LogicalLine(line_number, buf)));
                    }
                    if count_ending_backslashes(&line) % 2 == 1 {
                        buf.pop();
                    } else {
                        assert!(line_number != 0);
                        return Some(Ok(LogicalLine(line_number, buf)));
                    }
                }
                None => {
                    self.eof = true;
                    return None;
                }
            }
            first = false;
        }
    }
}

/////////////////////

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ParsedLine<'a> {
    Comment(&'a str),
    KVPair(&'a str, &'a str),
}

/// A line read from a properties file.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Hash)]
pub struct Line {
    line_number: usize,
    data: LineContent,
}

impl Line {
    /// Returns the 1-based line number.
    pub fn line_number(&self) -> usize {
        self.line_number
    }

    /// Returns the content of the line.
    pub fn content(&self) -> &LineContent {
        &self.data
    }

    /// Returns the content of the line, consuming it in the process.
    pub fn consume_content(self) -> LineContent {
        self.data
    }

    fn mk_pair(line_number: usize, key: String, value: String) -> Line {
        Line {
            line_number,
            data: LineContent::KVPair(key, value),
        }
    }

    fn mk_comment(line_number: usize, text: String) -> Line {
        Line {
            line_number,
            data: LineContent::Comment(text),
        }
    }
}

impl Display for Line {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Line {{line_number: {}, content: {}}}",
            self.line_number, self.data
        )
    }
}

/// Parsed content of the line.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone, Hash)]
pub enum LineContent {
    /// Content of a comment line.
    Comment(String),

    /// Content of a key/value line.
    KVPair(String, String),
}

impl Display for LineContent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            LineContent::Comment(ref s) => write!(f, "Comment({:?})", s),
            LineContent::KVPair(ref k, ref v) => write!(f, "KVPair({:?}, {:?})", k, v),
        }
    }
}

impl From<Line> for LineContent {
    fn from(line: Line) -> LineContent {
        line.data
    }
}

/////////////////////

fn unescape(s: &str, line_number: usize) -> Result<String, PropertiesError> {
    let mut buf = String::new();
    let mut iter = s.chars();
    loop {
        match iter.next() {
            None => break,
            Some(c) => {
                if c == '\\' {
                    match iter.next() {
                        Some(c) => {
                            match c {
                                // \b is specifically blacklisted by the documentation.  Why?  Who knows.
                                't' => buf.push('\t'),
                                'n' => buf.push('\n'),
                                'f' => buf.push('\x0c'),
                                'r' => buf.push('\r'),
                                'u' => {
                                    let mut tmp = String::new();
                                    for _ in 0..4 {
                                        match iter.next() {
                                            Some(c) => tmp.push(c),
                                            None => return Err(PropertiesError::new(
                                                "Malformed \\uxxxx encoding: not enough digits.",
                                                None,
                                                Some(line_number),
                                            )),
                                        }
                                    }
                                    let val = match u16::from_str_radix(&tmp, 16) {
                                        Ok(x) => x,
                                        Err(e) => {
                                            return Err(PropertiesError::new(
                                                "Malformed \\uxxxx encoding: not hex.",
                                                Some(Box::new(e)),
                                                Some(line_number),
                                            ))
                                        }
                                    };
                                    match std::char::from_u32(val as u32) {
                                        Some(c) => buf.push(c),
                                        None => {
                                            return Err(PropertiesError::new(
                                                "Malformed \\uxxxx encoding: invalid character.",
                                                None,
                                                Some(line_number),
                                            ))
                                        }
                                    }
                                }
                                _ => buf.push(c),
                            }
                        }
                        None => {
                            // The Java implementation replaces a dangling backslash with a NUL byte (\0).
                            // Is this "correct"?  Probably not.
                            // It's never documented, so assume it's undefined behavior.
                            // Let's do what Java does, though.
                            buf.push('\x00');
                            break;
                        }
                    }
                } else {
                    buf.push(c);
                }
            }
        }
    }
    Ok(buf)
}

lazy_static! {
  // Note that we have to use \x20 to match a space and \x23 to match a pound character since we're ignoring whitespace and comments
  static ref LINE_RE: Regex = Regex::new(r"(?x) # allow whitespace and comments
      ^
      [\x20\t\r\n\x0c]* # ignorable whitespace
      (?:
        [\x23!] # start of comment (# or !)
        [\x20\t\r\n\x0c]* # ignorable whitespace
        (.*?) # comment text
        [\x20\t\r\n\x0c]* # ignorable whitespace
      |
        (
          (?:[^\\:=\x20\t\r\n\x0c]|\\.)* # key
          (?:\\$)? # end of line backslash, can't show up in real input because it's caught by LogicalLines
        )
        (?:
          (?:
            [\x20\t\r\n\x0c]*[:=][\x20\t\r\n\x0c]* # try matching an actual separator (: or =)
          |
            [\x20\t\r\n\x0c]+ # try matching whitespace only
          )
          (
            (?:[^\\]|\\.)*? # value
            (?:\\$)? # end of line backslash, can't show up in real input because it's caught by LogicalLines
          )
        )?
      )
      $
    ").unwrap();
}

fn parse_line(line: &str) -> Option<ParsedLine> {
    if let Some(c) = LINE_RE.captures(line) {
        if let Some(comment_match) = c.get(1) {
            Some(ParsedLine::Comment(comment_match.as_str()))
        } else if let Some(key_match) = c.get(2) {
            let key = key_match.as_str();
            if let Some(value_match) = c.get(3) {
                Some(ParsedLine::KVPair(key, value_match.as_str()))
            } else if !key.is_empty() {
                Some(ParsedLine::KVPair(key, ""))
            } else {
                None
            }
        } else {
            panic!("Failed to get any groups out of the regular expression.")
        }
    } else {
        // This should never happen.  The pattern should match all strings.
        panic!("Failed to match on {:?}", line);
    }
}

/// Parses a properties file and iterates over its contents.
///
/// For basic usage, see the crate-level documentation.
/// Note that once `next` returns an error, the result of further calls is undefined.
pub struct PropertiesIter<R: Read> {
    lines: LogicalLines<NaturalLines<R>>,
}

impl<R: Read> PropertiesIter<R> {
    /// Parses properties from the given `Read` stream.
    pub fn new(input: R) -> Self {
        Self::new_with_encoding(input, WINDOWS_1252)
    }

    /// Parses properties from the given `Read` stream in the given encoding.
    /// Note that the Java properties specification specifies ISO-8859-1 encoding
    /// (a.k.a. windows-1252) for properties files; in most cases, `new` should be
    /// called instead.
    pub fn new_with_encoding(input: R, encoding: &'static Encoding) -> Self {
        PropertiesIter {
            lines: LogicalLines::new(NaturalLines::new(input, encoding)),
        }
    }

    /// Calls `f` for each key/value pair.
    ///
    /// Line numbers and comments are ignored.
    /// On the first error, the error is returned.
    /// Note that `f` may have already been called at this point.
    pub fn read_into<F: FnMut(String, String)>(&mut self, mut f: F) -> Result<(), PropertiesError> {
        for line in self {
            if let LineContent::KVPair(key, value) = line?.data {
                f(key, value);
            }
        }
        Ok(())
    }

    fn parsed_line_to_line(
        &self,
        parsed_line: ParsedLine<'_>,
        line_number: usize,
    ) -> Result<Line, PropertiesError> {
        Ok(match parsed_line {
            ParsedLine::Comment(c) => {
                let comment = unescape(c, line_number)?;
                Line::mk_comment(line_number, comment)
            }
            ParsedLine::KVPair(k, v) => {
                let key = unescape(k, line_number)?;
                let value = unescape(v, line_number)?;
                Line::mk_pair(line_number, key, value)
            }
        })
    }
}

/// Note that once `next` returns an error, the result of further calls is undefined.
impl<R: Read> Iterator for PropertiesIter<R> {
    type Item = Result<Line, PropertiesError>;

    /// Returns the next line.
    ///
    /// Once this returns an error, the result of further calls is undefined.
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.lines.next() {
                Some(Ok(LogicalLine(line_no, line))) => {
                    if let Some(parsed_line) = parse_line(&line) {
                        return Some(self.parsed_line_to_line(parsed_line, line_no));
                    }
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

/////////////////////

/// A line ending style allowed in a Java properties file.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, Hash)]
pub enum LineEnding {
    /// Carriage return alone.
    CR,
    /// Line feed alone.
    LF,
    /// Carriage return followed by line feed.
    // The name can't be changed without breaking backward compatibility.
    #[allow(clippy::upper_case_acronyms)]
    CRLF,
}

impl Display for LineEnding {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(match *self {
            LineEnding::CR => "LineEnding::CR",
            LineEnding::LF => "LineEnding::LF",
            LineEnding::CRLF => "LineEnding::CRLF",
        })
    }
}

struct EncodingWriter<W: Write> {
    writer: W,
    lines_written: usize,
    encoder: Encoder,
    buffer: Vec<u8>,
}

impl<W: Write> EncodingWriter<W> {
    fn write(&mut self, mut data: &str) -> Result<(), PropertiesError> {
        while !data.is_empty() {
            let (result, bytes_read) = self.encoder.encode_from_utf8_to_vec_without_replacement(
                data,
                &mut self.buffer,
                false,
            );
            data = &data[bytes_read..];
            match result {
                EncoderResult::InputEmpty => (),
                EncoderResult::OutputFull => {
                    // encoding_rs won't just append to vectors if it would require allocation.
                    self.buffer.reserve(self.buffer.capacity() * 2);
                }
                EncoderResult::Unmappable(c) => {
                    let escaped = format!("\\u{:x}", c as isize);
                    let (result2, _) = self.encoder.encode_from_utf8_to_vec_without_replacement(
                        &escaped,
                        &mut self.buffer,
                        false,
                    );
                    match result2 {
                        EncoderResult::InputEmpty => (),
                        EncoderResult::OutputFull => {
                            // encoding_rs won't just append to vectors if it would require allocation.
                            self.buffer.reserve(self.buffer.capacity() * 2);
                        }
                        EncoderResult::Unmappable(_) => {
                            return Err(PropertiesError::new(
                                format!(
                                    "Encoding error: unable to write UTF-8 escaping {:?} for {:?}",
                                    escaped, c
                                ),
                                None,
                                Some(self.lines_written),
                            ))
                        }
                    }
                }
            }
        }
        self.flush_buffer()?;
        Ok(())
    }

    fn flush_buffer(&mut self) -> Result<(), PropertiesError> {
        self.writer.write_all(&self.buffer).map_err(|e| {
            PropertiesError::new("I/O error", Some(Box::new(e)), Some(self.lines_written))
        })?;
        self.buffer.clear();
        Ok(())
    }

    fn flush(&mut self) -> Result<(), PropertiesError> {
        self.flush_buffer()?;
        self.writer.flush()?;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), PropertiesError> {
        let (result, _) =
            self.encoder
                .encode_from_utf8_to_vec_without_replacement("", &mut self.buffer, true);
        match result {
            EncoderResult::InputEmpty => (),
            EncoderResult::OutputFull => {
                return Err(PropertiesError::new(
                    "Encoding error: output full",
                    None,
                    Some(self.lines_written),
                ))
            }
            EncoderResult::Unmappable(c) => {
                return Err(PropertiesError::new(
                    format!("Encoding error: unmappable character {:?}", c),
                    None,
                    Some(self.lines_written),
                ))
            }
        }
        self.flush()?;
        Ok(())
    }
}

/// Writes to a properties file.
///
/// `finish()` *must* be called after writing all data.
pub struct PropertiesWriter<W: Write> {
    comment_prefix: String,
    kv_separator: String,
    line_ending: LineEnding,
    writer: EncodingWriter<W>,
}

impl<W: Write> PropertiesWriter<W> {
    /// Writes to the given `Write` stream.
    pub fn new(writer: W) -> Self {
        Self::new_with_encoding(writer, WINDOWS_1252)
    }

    /// Writes to the given `Write` stream in the given encoding.
    /// Note that the Java properties specification specifies ISO-8859-1 encoding
    /// for properties files; in most cases, `new` should be called instead.
    pub fn new_with_encoding(writer: W, encoding: &'static Encoding) -> Self {
        PropertiesWriter {
            comment_prefix: "# ".to_string(),
            kv_separator: "=".to_string(),
            line_ending: LineEnding::LF,
            writer: EncodingWriter {
                writer,
                lines_written: 0,
                encoder: encoding.new_encoder(),
                // It's important that we start with a non-zero capacity, since we double it as needed.
                buffer: Vec::with_capacity(256),
            },
        }
    }

    fn write_eol(&mut self) -> Result<(), PropertiesError> {
        self.writer.write(match self.line_ending {
            LineEnding::CR => "\r",
            LineEnding::LF => "\n",
            LineEnding::CRLF => "\r\n",
        })?;
        Ok(())
    }

    /// Writes a comment to the file.
    pub fn write_comment(&mut self, comment: &str) -> Result<(), PropertiesError> {
        self.writer.lines_written += 1;
        self.writer.write(&self.comment_prefix)?;
        self.writer.write(comment)?;
        self.write_eol()?;
        Ok(())
    }

    fn write_escaped(&mut self, s: &str) -> Result<(), PropertiesError> {
        self.writer.lines_written += 1;
        let mut escaped = String::new();
        for c in s.chars() {
            match c {
                '\\' => escaped.push_str("\\\\"),
                ' ' => escaped.push_str("\\ "),
                '\t' => escaped.push_str("\\t"),
                '\r' => escaped.push_str("\\r"),
                '\n' => escaped.push_str("\\n"),
                '\x0c' => escaped.push_str("\\f"),
                ':' => escaped.push_str("\\:"),
                '=' => escaped.push_str("\\="),
                '!' => escaped.push_str("\\!"),
                '#' => escaped.push_str("\\#"),
                _ if c < ' ' => escaped.push_str(&format!("\\u{:x}", c as u16)),
                _ => escaped.push(c), // We don't worry about other characters, since they're taken care of below.
            }
        }
        self.writer.write(&escaped)?;
        Ok(())
    }

    /// Writes a key/value pair to the file.
    pub fn write(&mut self, key: &str, value: &str) -> Result<(), PropertiesError> {
        self.write_escaped(key)?;
        self.writer.write(&self.kv_separator)?;
        self.write_escaped(value)?;
        self.write_eol()?;
        Ok(())
    }

    /// Flushes the underlying stream.
    pub fn flush(&mut self) -> Result<(), PropertiesError> {
        self.writer.flush()?;
        Ok(())
    }

    /// Sets the comment prefix.
    ///
    /// The prefix must contain a '#' or a '!', may only contain spaces, tabs, or form feeds before the comment character,
    /// and may not contain any carriage returns or line feeds ('\r' or '\n').
    pub fn set_comment_prefix(&mut self, prefix: &str) -> Result<(), PropertiesError> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r"^[ \t\x0c]*[#!][^\r\n]*$").unwrap();
        }
        if !RE.is_match(prefix) {
            return Err(PropertiesError::new(
                &format!("Bad comment prefix: {:?}", prefix),
                None,
                None,
            ));
        }
        self.comment_prefix = prefix.to_string();
        Ok(())
    }

    /// Sets the key/value separator.
    ///
    /// The separator may be non-empty whitespace, or a colon with optional whitespace on either side,
    /// or an equals sign with optional whitespace on either side.  (Whitespace here means ' ', '\t', or '\f'.)
    pub fn set_kv_separator(&mut self, separator: &str) -> Result<(), PropertiesError> {
        lazy_static! {
            static ref RE: Regex = Regex::new(r"^([ \t\x0c]*[:=][ \t\x0c]*|[ \t\x0c]+)$").unwrap();
        }
        if !RE.is_match(separator) {
            return Err(PropertiesError::new(
                &format!("Bad key/value separator: {:?}", separator),
                None,
                None,
            ));
        }
        self.kv_separator = separator.to_string();
        Ok(())
    }

    /// Sets the line ending.
    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    /// Finishes the encoding.
    pub fn finish(&mut self) -> Result<(), PropertiesError> {
        self.writer.finish()?;
        Ok(())
    }
}

/////////////////////

/// Writes a hash map to a properties file.
///
/// For more advanced use cases, use `PropertiesWriter`.
pub fn write<W: Write>(writer: W, map: &HashMap<String, String>) -> Result<(), PropertiesError> {
    let mut writer = PropertiesWriter::new(writer);
    for (k, v) in map {
        writer.write(k, v)?;
    }
    writer.finish()?;
    Ok(())
}

/// Reads a properties file into a hash map.
///
/// For more advanced use cases, use `PropertiesIter`.
pub fn read<R: Read>(input: R) -> Result<HashMap<String, String>, PropertiesError> {
    let mut p = PropertiesIter::new(input);
    let mut map = HashMap::new();
    p.read_into(|k, v| {
        map.insert(k, v);
    })?;
    Ok(map)
}

/////////////////////

#[cfg(test)]
mod tests {
    use super::Line;
    use super::LineEnding;
    use super::LogicalLine;
    use super::LogicalLines;
    use super::NaturalLine;
    use super::NaturalLines;
    use super::ParsedLine;
    use super::PropertiesError;
    use super::PropertiesIter;
    use super::PropertiesWriter;
    use encoding_rs::UTF_8;
    use encoding_rs::WINDOWS_1252;
    use std::io;
    use std::io::ErrorKind;
    use std::io::Read;

    const LF: u8 = b'\n';
    const CR: u8 = b'\r';
    const SP: u8 = b' '; // space

    #[test]
    fn natural_lines() {
        let data = [
            (vec![], vec![""]),
            (vec![SP], vec![" "]),
            (vec![SP, CR], vec![" ", ""]),
            (vec![SP, LF], vec![" ", ""]),
            (vec![SP, CR, LF], vec![" ", ""]),
            (vec![SP, CR, SP], vec![" ", " "]),
            (vec![SP, LF, SP], vec![" ", " "]),
            (vec![SP, CR, LF, SP], vec![" ", " "]),
            (vec![CR], vec!["", ""]),
            (vec![LF], vec!["", ""]),
            (vec![CR, LF], vec!["", ""]),
            (vec![CR, SP], vec!["", " "]),
            (vec![LF, SP], vec!["", " "]),
            (vec![CR, LF, SP], vec!["", " "]),
        ];
        for &(ref bytes, ref lines) in &data {
            let reader = &bytes as &[u8];
            let mut iter = NaturalLines::new(reader, WINDOWS_1252);
            let mut count = 1;
            for line in lines {
                match (line.to_string(), iter.next()) {
                    (ref e, Some(Ok(NaturalLine(a_ln, ref a)))) => {
                        if (count, e) != (a_ln, a) {
                            panic!("Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}", bytes, (count, e), (a_ln, a));
                        }
                    }
                    (e, a) => panic!(
                        "Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}",
                        bytes,
                        (count, e),
                        a
                    ),
                }
                count += 1;
            }
            match iter.next() {
                None => (),
                a => panic!(
                    "Failure while processing {:?}.  Expected None, but was {:?}",
                    bytes, a
                ),
            }
        }
    }

    #[test]
    fn logical_lines() {
        let data = [
            (vec![], vec![]),
            (vec!["foo"], vec!["foo"]),
            (vec!["foo", "bar"], vec!["foo", "bar"]),
            (vec!["foo\\", "bar"], vec!["foobar"]),
            (vec!["foo\\\\", "bar"], vec!["foo\\\\", "bar"]),
            (vec!["foo\\\\\\", "bar"], vec!["foo\\\\bar"]),
            (vec!["foo\\", " bar"], vec!["foobar"]),
            (vec!["#foo\\", " bar"], vec!["#foo\\", " bar"]),
            (vec!["foo\\", "# bar"], vec!["foo# bar"]),
            (vec!["\u{1F41E}\\", "\u{1F41E}"], vec!["\u{1F41E}\u{1F41E}"]),
            (
                vec!["\u{1F41E}\\", " \u{1F41E}"],
                vec!["\u{1F41E}\u{1F41E}"],
            ),
        ];
        for &(ref input_lines, ref lines) in &data {
            let mut count = 0;
            let mut iter = LogicalLines::new(input_lines.iter().map(|x| {
                count += 1;
                Ok(NaturalLine(count, x.to_string()))
            }));
            let mut e_ln = 0;
            for line in lines {
                e_ln += 1;
                match (line.to_string(), iter.next()) {
                    (ref e, Some(Ok(LogicalLine(a_ln, ref a)))) => {
                        if (e_ln, e) != (a_ln, a) {
                            panic!("Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}", input_lines, (e_ln, e), (a_ln, a));
                        }
                    }
                    (e, a) => panic!(
                        "Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}",
                        input_lines,
                        (e_ln, e),
                        a
                    ),
                }
            }
            match iter.next() {
                None => (),
                a => panic!(
                    "Failure while processing {:?}.  Expected None, but was {:?}",
                    input_lines, a
                ),
            }
        }
    }

    #[test]
    fn count_ending_backslashes() {
        assert_eq!(0, super::count_ending_backslashes(""));

        assert_eq!(0, super::count_ending_backslashes("x"));
        assert_eq!(1, super::count_ending_backslashes("\\"));

        assert_eq!(0, super::count_ending_backslashes("xx"));
        assert_eq!(0, super::count_ending_backslashes("\\x"));
        assert_eq!(1, super::count_ending_backslashes("x\\"));
        assert_eq!(2, super::count_ending_backslashes("\\\\"));

        assert_eq!(0, super::count_ending_backslashes("xxx"));
        assert_eq!(0, super::count_ending_backslashes("\\xx"));
        assert_eq!(0, super::count_ending_backslashes("x\\x"));
        assert_eq!(0, super::count_ending_backslashes("\\\\x"));
        assert_eq!(1, super::count_ending_backslashes("xx\\"));
        assert_eq!(1, super::count_ending_backslashes("\\x\\"));
        assert_eq!(2, super::count_ending_backslashes("x\\\\"));
        assert_eq!(3, super::count_ending_backslashes("\\\\\\"));

        assert_eq!(0, super::count_ending_backslashes("x\u{1F41E}"));
        assert_eq!(0, super::count_ending_backslashes("\\\u{1F41E}"));
        assert_eq!(0, super::count_ending_backslashes("\u{1F41E}x"));
        assert_eq!(1, super::count_ending_backslashes("\u{1F41E}\\"));
    }

    #[test]
    fn parse_line() {
        let data = [
            ("", None),
            (" ", None),
            ("\\", Some(ParsedLine::KVPair("\\", ""))),
            ("a=\\", Some(ParsedLine::KVPair("a", "\\"))),
            ("\\ ", Some(ParsedLine::KVPair("\\ ", ""))),
            ("# foo", Some(ParsedLine::Comment("foo"))),
            (" # foo", Some(ParsedLine::Comment("foo"))),
            ("a # foo", Some(ParsedLine::KVPair("a", "# foo"))),
            ("a", Some(ParsedLine::KVPair("a", ""))),
            ("a = b", Some(ParsedLine::KVPair("a", "b"))),
            ("a : b", Some(ParsedLine::KVPair("a", "b"))),
            ("a b", Some(ParsedLine::KVPair("a", "b"))),
            (" a = b ", Some(ParsedLine::KVPair("a", "b "))),
            (" a : b", Some(ParsedLine::KVPair("a", "b"))),
            (" a b", Some(ParsedLine::KVPair("a", "b"))),
            ("a:=b", Some(ParsedLine::KVPair("a", "=b"))),
            ("a=:b", Some(ParsedLine::KVPair("a", ":b"))),
            ("a b:c", Some(ParsedLine::KVPair("a", "b:c"))),
            (
                "a\\ \\:\\=b c",
                Some(ParsedLine::KVPair("a\\ \\:\\=b", "c")),
            ),
            (
                "a\\ \\:\\=b=c",
                Some(ParsedLine::KVPair("a\\ \\:\\=b", "c")),
            ),
            (
                "a\\\\ \\\\:\\\\=b c",
                Some(ParsedLine::KVPair("a\\\\", "\\\\:\\\\=b c")),
            ),
            ("\\  b", Some(ParsedLine::KVPair("\\ ", "b"))),
            ("=", Some(ParsedLine::KVPair("", ""))),
            ("=x", Some(ParsedLine::KVPair("", "x"))),
            ("x=", Some(ParsedLine::KVPair("x", ""))),
            ("\\=x", Some(ParsedLine::KVPair("\\=x", ""))),
            (
                "\u{1F41E}=\u{1F41E}",
                Some(ParsedLine::KVPair("\u{1F41E}", "\u{1F41E}")),
            ),
        ];
        for &(line, ref expected) in &data {
            let actual = super::parse_line(line);
            if expected != &actual {
                panic!(
                    "Failed when splitting {:?}.  Expected {:?} but got {:?}",
                    line, expected, actual
                );
            }
        }
    }

    #[test]
    fn unescape() {
        let data = [
            (r"", Some("")),
            (r"x", Some("x")),
            (r"\\", Some("\\")),
            (r"\#", Some("#")),
            (r"\!", Some("!")),
            (r"\\\n\r\t\f\u0001\b", Some("\\\n\r\t\x0c\u{0001}b")),
            (r"\", Some("\x00")),
            (r"\u", None),
            (r"\uasfd", None),
        ];
        for &(input, expected) in &data {
            let actual = &super::unescape(input, 1);
            let is_match = match (expected, actual) {
                (Some(e), &Ok(ref a)) => e == a,
                (None, &Err(_)) => true,
                _ => false,
            };
            if !is_match {
                panic!(
                    "Failed when unescaping {:?}.  Expected {:?} but got {:?}",
                    input, expected, actual
                );
            }
        }
    }

    #[test]
    fn properties_iter() {
        fn mk_comment(line_no: usize, text: &str) -> Line {
            Line::mk_comment(line_no, text.to_string())
        }
        fn mk_pair(line_no: usize, key: &str, value: &str) -> Line {
            Line::mk_pair(line_no, key.to_string(), value.to_string())
        }
        let data = vec![
            (
                WINDOWS_1252,
                vec![
      ("", vec![]),
      ("a=b", vec![mk_pair(1, "a", "b")]),
      ("a=\\#b", vec![mk_pair(1, "a", "#b")]),
      ("\\!a=b", vec![mk_pair(1, "!a", "b")]),
      ("a=b\nc=d\\\ne=f\ng=h\r#comment1\r\n#comment2\\\ni=j\\\n#comment3\n \n#comment4", vec![
        mk_pair(1, "a", "b"),
        mk_pair(2, "c", "de=f"),
        mk_pair(4, "g", "h"),
        mk_comment(5, "comment1"),
        mk_comment(6, "comment2\u{0}"),
        mk_pair(7, "i", "j#comment3"),
        mk_comment(10, "comment4"),
      ]),
      ("a = b\\\n  c, d ", vec![mk_pair(1, "a", "bc, d ")]),
      ("x=\\\\\\\nty", vec![mk_pair(1, "x", "\\ty")]),
    ],
            ),
            (
                UTF_8,
                vec![(
                    "a=日本語\nb=Français",
                    vec![mk_pair(1, "a", "日本語"), mk_pair(2, "b", "Français")],
                )],
            ),
        ];
        for &(encoding, ref dataset) in &data {
            for &(input, ref lines) in dataset {
                let mut iter = PropertiesIter::new_with_encoding(input.as_bytes(), encoding);
                for line in lines {
                    match (line, iter.next()) {
                        (ref e, Some(Ok(ref a))) => {
                            if e != &a {
                                panic!("Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}", input, e, a);
                            }
                        }
                        (e, a) => panic!(
                            "Failure while processing {:?}.  Expected Some(Ok({:?})), but was {:?}",
                            input, e, a
                        ),
                    }
                }
                match iter.next() {
                    None => (),
                    a => panic!(
                        "Failure while processing {:?}.  Expected None, but was {:?}",
                        input, a
                    ),
                }
            }
        }
    }

    #[test]
    fn properties_writer_kv() {
        let data = [
            ("", "", "=\n"),
            ("a", "b", "a=b\n"),
            (" :=", " :=", "\\ \\:\\==\\ \\:\\=\n"),
            ("!", "#", "\\!=\\#\n"),
            ("\u{1F41E}", "\u{1F41E}", "\\u1f41e=\\u1f41e\n"),
        ];
        for &(key, value, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new(&mut buf);
                writer.write(key, value).unwrap();
                writer.finish().unwrap();
            }
            let actual = WINDOWS_1252.decode(&buf).0;
            if expected != actual {
                panic!("Failure while processing key {:?} and value {:?}.  Expected {:?}, but was {:?}", key, value, expected, actual);
            }
        }
    }

    #[test]
    fn properties_writer_kv_custom_encoding() {
        let data = [
            ("", "", "=\n"),
            ("a", "b", "a=b\n"),
            (" :=", " :=", "\\ \\:\\==\\ \\:\\=\n"),
            ("!", "#", "\\!=\\#\n"),
            ("\u{1F41E}", "\u{1F41E}", "\u{1F41E}=\u{1F41E}\n"),
        ];
        for &(key, value, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new_with_encoding(&mut buf, UTF_8);
                writer.write(key, value).unwrap();
                writer.finish().unwrap();
            }
            let actual = UTF_8.decode(&buf).0;
            if expected != actual {
                panic!("Failure while processing key {:?} and value {:?}.  Expected {:?}, but was {:?}", key, value, expected, actual);
            }
        }
    }

    #[test]
    fn properties_writer_comment() {
        let data = [
            ("", "# \n"),
            ("a", "# a\n"),
            (" :=", "#  :=\n"),
            ("\u{1F41E}", "# \\u1f41e\n"),
        ];
        for &(comment, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new(&mut buf);
                writer.write_comment(comment).unwrap();
                writer.finish().unwrap();
            }
            let actual = UTF_8.decode(&buf).0;
            if expected != actual {
                panic!(
                    "Failure while processing {:?}.  Expected {:?}, but was {:?}",
                    comment, expected, actual
                );
            }
        }
    }

    #[test]
    fn properties_writer_good_comment_prefix() {
        let prefixes = ["#", "!", " #", " !", "#x", "!x", "\x0c#"];
        let mut buf = Vec::new();
        for prefix in &prefixes {
            let mut writer = PropertiesWriter::new(&mut buf);
            writer.set_comment_prefix(prefix).unwrap();
        }
    }

    #[test]
    fn properties_writer_bad_comment_prefix() {
        let prefixes = ["", " ", "x", "\n#", "#\n", "#\r"];
        let mut buf = Vec::new();
        for prefix in &prefixes {
            let mut writer = PropertiesWriter::new(&mut buf);
            match writer.set_comment_prefix(prefix) {
                Ok(_) => panic!("Unexpectedly succeded with prefix {:?}", prefix),
                Err(_) => (),
            }
        }
    }

    #[test]
    fn properties_writer_custom_comment_prefix() {
        let data = [
            ("", " !\n"),
            ("a", " !a\n"),
            (" :=", " ! :=\n"),
            ("\u{1F41E}", " !\\u1f41e\n"),
        ];
        for &(comment, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new(&mut buf);
                writer.set_comment_prefix(" !").unwrap();
                writer.write_comment(comment).unwrap();
                writer.finish().unwrap();
            }
            let actual = WINDOWS_1252.decode(&buf).0;
            if expected != actual {
                panic!(
                    "Failure while processing {:?}.  Expected {:?}, but was {:?}",
                    comment, expected, actual
                );
            }
        }
    }

    #[test]
    fn properties_writer_good_kv_separator() {
        let separators = [":", "=", " ", " :", ": ", " =", "= ", "\x0c", "\t"];
        let mut buf = Vec::new();
        for separator in &separators {
            let mut writer = PropertiesWriter::new(&mut buf);
            writer.set_kv_separator(separator).unwrap();
        }
    }

    #[test]
    fn properties_writer_bad_kv_separator() {
        let separators = ["", "x", ":=", "=:", "\n", "\r"];
        let mut buf = Vec::new();
        for separator in &separators {
            let mut writer = PropertiesWriter::new(&mut buf);
            match writer.set_kv_separator(separator) {
                Ok(_) => panic!("Unexpectedly succeded with separator {:?}", separator),
                Err(_) => (),
            }
        }
    }

    #[test]
    fn properties_writer_custom_kv_separator() {
        let data = [
            (":", "x:y\n"),
            ("=", "x=y\n"),
            (" ", "x y\n"),
            (" :", "x :y\n"),
            (": ", "x: y\n"),
            (" =", "x =y\n"),
            ("= ", "x= y\n"),
            ("\x0c", "x\x0cy\n"),
            ("\t", "x\ty\n"),
        ];
        for &(separator, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new(&mut buf);
                writer.set_kv_separator(separator).unwrap();
                writer.write("x", "y").unwrap();
                writer.finish().unwrap();
            }
            let actual = WINDOWS_1252.decode(&buf).0;
            if expected != actual {
                panic!(
                    "Failure while processing {:?}.  Expected {:?}, but was {:?}",
                    separator, expected, actual
                );
            }
        }
    }

    #[test]
    fn properties_writer_custom_line_ending() {
        let data = [
            (LineEnding::CR, "# foo\rx=y\r"),
            (LineEnding::LF, "# foo\nx=y\n"),
            (LineEnding::CRLF, "# foo\r\nx=y\r\n"),
        ];
        for &(line_ending, expected) in &data {
            let mut buf = Vec::new();
            {
                let mut writer = PropertiesWriter::new(&mut buf);
                writer.set_line_ending(line_ending);
                writer.write_comment("foo").unwrap();
                writer.write("x", "y").unwrap();
                writer.finish().unwrap();
            }
            let actual = WINDOWS_1252.decode(&buf).0;
            if expected != actual {
                panic!(
                    "Failure while processing {:?}.  Expected {:?}, but was {:?}",
                    line_ending, expected, actual
                );
            }
        }
    }

    struct ErrorReader;

    impl Read for ErrorReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(ErrorKind::InvalidData, "dummy error"))
        }
    }

    #[test]
    fn properties_error_line_number() {
        let data = [
            ("", 1),
            ("\n", 2),
            ("\r", 2),
            ("\r\n", 2),
            ("\\uxxxx", 1),
            ("\n\\uxxxx", 2),
            ("a\\\nb\n\\uxxxx", 3),
        ];
        for &(input, line_number) in &data {
            let iter = PropertiesIter::new(input.as_bytes().chain(ErrorReader));
            let mut got_error = false;
            for line in iter {
                if let Err(e) = line {
                    assert_eq!(e.line_number(), Some(line_number));
                    got_error = true;
                    break;
                }
            }
            assert!(got_error);
        }
    }

    #[test]
    fn properties_error_display() {
        assert_eq!(
            format!("{}", PropertiesError::new("foo", None, None)),
            "foo (line_number = unknown)"
        );
        assert_eq!(
            format!("{}", PropertiesError::new("foo", None, Some(1))),
            "foo (line_number = 1)"
        );
    }

    #[test]
    fn line_display() {
        assert_eq!(
            format!("{}", Line::mk_pair(1, "foo".to_string(), "bar".to_string())),
            "Line {line_number: 1, content: KVPair(\"foo\", \"bar\")}"
        );
        assert_eq!(
            format!("{}", Line::mk_comment(1, "baz".to_string())),
            "Line {line_number: 1, content: Comment(\"baz\")}"
        );
    }
}
