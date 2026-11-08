//! Minimal Java class-file reader used for the no-source fallback.
//!
//! This is deliberately not a decompiler: it only reads the constant pool and
//! the field/method tables so a readable structure view can be produced offline.
//! Java 8 class files (major <= 52) are the primary target, but newer versions
//! parse as long as no unknown constant-pool tags appear.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassOutline {
    pub name: String,
    pub super_name: String,
    pub fields: Vec<Member>,
    pub methods: Vec<Member>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn u1(&mut self) -> Option<u8> {
        let value = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(value)
    }

    fn u2(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u1()?, self.u1()?]))
    }

    fn u4(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes([
            self.u1()?,
            self.u1()?,
            self.u1()?,
            self.u1()?,
        ]))
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.pos = self.pos.checked_add(count)?;
        (self.pos <= self.data.len()).then_some(())
    }
}

pub fn outline(bytes: &[u8]) -> Option<ClassOutline> {
    let mut reader = Reader::new(bytes);
    if reader.u4()? != 0xCAFE_BABE {
        return None;
    }
    let _minor = reader.u2()?;
    let _major = reader.u2()?;
    let count = reader.u2()? as usize;
    let mut utf8: Vec<Option<String>> = Vec::with_capacity(count);
    utf8.push(None); // constant pool is 1-based and slot 0 is unused

    let mut index = 1;
    while index < count {
        let tag = reader.u1()?;
        match tag {
            1 => {
                let length = reader.u2()? as usize;
                let end = reader.pos.checked_add(length)?;
                let raw = bytes.get(reader.pos..end)?;
                utf8.push(Some(String::from_utf8_lossy(raw).into_owned()));
                reader.pos = end;
            }
            3 | 4 => {
                reader.skip(4)?;
                utf8.push(None);
            }
            5 | 6 => {
                reader.skip(8)?;
                utf8.push(None);
                utf8.push(None); // long/double occupy two slots
                index += 1;
            }
            7 | 8 | 16 | 19 | 20 => {
                reader.skip(2)?;
                utf8.push(None);
            }
            9 | 10 | 11 | 12 | 17 | 18 => {
                reader.skip(4)?;
                utf8.push(None);
            }
            15 => {
                reader.skip(3)?;
                utf8.push(None);
            }
            _ => return None,
        }
        index += 1;
    }

    let text = |index: u16, pool: &[Option<String>]| -> Option<String> {
        pool.get(index as usize).and_then(|value| value.clone())
    };

    let _access = reader.u2()?;
    let this_class = reader.u2()?;
    let super_class = reader.u2()?;
    let name = text(this_class, &utf8)
        .unwrap_or_default()
        .replace('/', ".");
    let super_name = if super_class == 0 {
        String::new()
    } else {
        text(super_class, &utf8)
            .unwrap_or_default()
            .replace('/', ".")
    };
    let interfaces = reader.u2()? as usize;
    reader.skip(interfaces * 2)?;

    let fields = read_members(&mut reader, &utf8)?;
    let methods = read_members(&mut reader, &utf8)?;
    Some(ClassOutline {
        name,
        super_name,
        fields,
        methods,
    })
}

fn read_members(reader: &mut Reader<'_>, utf8: &[Option<String>]) -> Option<Vec<Member>> {
    let count = reader.u2()? as usize;
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        let _access = reader.u2()?;
        let name_index = reader.u2()?;
        let descriptor_index = reader.u2()?;
        members.push(Member {
            name: utf8
                .get(name_index as usize)
                .and_then(|value| value.clone())
                .unwrap_or_default(),
            descriptor: utf8
                .get(descriptor_index as usize)
                .and_then(|value| value.clone())
                .unwrap_or_default(),
        });
        let attributes = reader.u2()? as usize;
        for _ in 0..attributes {
            let _attribute_name = reader.u2()?;
            let length = reader.u4()? as usize;
            reader.skip(length)?;
        }
    }
    Some(members)
}

/// Human-readable structure view; each member gets its own line so LSP can jump to it.
pub fn render(outline: &ClassOutline, jar_label: &str) -> String {
    let mut lines = vec![
        "// Structure view (no sources jar found; bytecode outline only)".to_string(),
        format!("// jar: {jar_label}"),
        "// A decompiler can be plugged in later via the Decompiler trait.".to_string(),
        String::new(),
    ];
    if outline.super_name.is_empty() {
        lines.push(format!("class {} {{", outline.name));
    } else {
        lines.push(format!(
            "class {} extends {} {{",
            outline.name, outline.super_name
        ));
    }
    for field in &outline.fields {
        lines.push(format!("    {} : {};", field.name, field.descriptor));
    }
    for method in &outline.methods {
        lines.push(format!(
            "    {}{} {{ ... }}",
            method.name, method.descriptor
        ));
    }
    lines.push("}".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_class_files() {
        assert!(outline(b"not a class file").is_none());
    }
}
