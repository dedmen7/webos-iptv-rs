#[derive(Default, PartialEq)]
enum State {
    #[default]
    Start,
    First,
    Next,
    Value,
    End,
}

#[derive(Default)]
pub(crate) struct JsonArray {
    state: State,
    pending: Vec<u8>,
    depth: usize,
    quoted: bool,
    escaped: bool,
}

impl JsonArray {
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        mut visit: impl FnMut(&[u8]) -> Result<bool, ()>,
    ) -> Result<bool, ()> {
        let mut start = 0;
        let mut index = 0;
        while index < bytes.len() {
            // Only quotes and escapes matter until the end of a string.
            if self.quoted && !self.escaped {
                let Some(special) = memchr::memchr2(b'"', b'\\', &bytes[index..]) else {
                    break;
                };
                index += special;
            }
            let byte = bytes[index];
            match self.state {
                State::Start if byte == b'[' => self.state = State::First,
                State::Start | State::End if !whitespace(byte) => return Err(()),
                State::First if byte == b']' => self.state = State::End,
                State::First | State::Next if !whitespace(byte) => {
                    if matches!(byte, b',' | b']') {
                        return Err(());
                    }
                    start = index;
                    self.state = State::Value;
                    self.scan(byte);
                }
                State::Value => {
                    if !self.quoted && self.depth == 0 && matches!(byte, b',' | b']') {
                        let current = &bytes[start..index];
                        let more = if self.pending.is_empty() {
                            visit(current)?
                        } else {
                            self.pending.extend_from_slice(current);
                            let more = visit(&self.pending)?;
                            self.pending.clear();
                            more
                        };
                        self.state = if byte == b',' {
                            State::Next
                        } else {
                            State::End
                        };
                        if !more {
                            return Ok(false);
                        }
                    } else {
                        self.scan(byte);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        if self.state == State::Value {
            self.pending.extend_from_slice(&bytes[start..]);
        }
        Ok(true)
    }

    pub(crate) fn finished(&self) -> bool {
        self.state == State::End
    }

    fn scan(&mut self, byte: u8) {
        if self.quoted {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.quoted = false;
            }
        } else {
            match byte {
                b'"' => self.quoted = true,
                b'[' | b'{' => self.depth += 1,
                b']' | b'}' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
        }
    }
}

fn whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t')
}

#[cfg(test)]
mod tests {
    use super::JsonArray;
    use serde_json::{Value, json};

    #[test]
    fn emits_complete_rows_across_every_chunk_boundary() {
        let expected = json!([{"title": "é ] \" \\", "data": [[1, 2]]}, null, {"id": 17}]);
        let raw = expected.to_string();
        let mut parser = JsonArray::default();
        let mut actual = Vec::<Value>::new();
        for byte in raw.as_bytes() {
            assert!(
                parser
                    .feed(&[*byte], |row| {
                        actual.push(serde_json::from_slice(row).map_err(|_| ())?);
                        Ok(true)
                    })
                    .unwrap()
            );
        }
        assert!(parser.finished());
        assert_eq!(Value::Array(actual), expected);
    }

    #[test]
    fn preserves_long_strings_and_escapes_across_chunk_boundaries() {
        let padding = "abc,]é".repeat(10);
        let expected = json!([format!("{padding}\\\"{padding}"), {"data": [true, null]}, ""]);
        let raw = expected.to_string();
        for split in 0..=raw.len() {
            let mut parser = JsonArray::default();
            let mut actual = Vec::<Value>::new();
            for chunk in [&raw.as_bytes()[..split], &raw.as_bytes()[split..]] {
                assert!(
                    parser
                        .feed(chunk, |row| {
                            actual.push(serde_json::from_slice(row).map_err(|_| ())?);
                            Ok(true)
                        })
                        .unwrap()
                );
            }
            assert!(parser.finished());
            assert_eq!(Value::Array(actual), expected);
        }
    }

    #[test]
    fn rejects_truncated_and_trailing_catalog_data() {
        let mut parser = JsonArray::default();
        assert!(parser.feed(br#"[{"id":1},"#, |_| Ok(true)).is_ok());
        assert!(!parser.finished());
        assert!(parser.feed(b"]", |_| Ok(true)).is_err());
        let mut parser = JsonArray::default();
        assert!(parser.feed(b"[] trailing", |_| Ok(true)).is_err());
    }

    #[test]
    fn can_stop_after_the_preview_without_reading_the_rest() {
        let mut parser = JsonArray::default();
        assert!(
            !parser
                .feed(br#"[{"id":1},{"id":2}]"#, |_| Ok(false))
                .unwrap()
        );
        assert!(!parser.finished());
    }
}
