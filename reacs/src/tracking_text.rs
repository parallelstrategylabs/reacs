use serde::{Serialize, ser};
use std::fmt::{self, Display, Write};

const MAX_TEXT_BYTES: usize = 240;
const ENTITY_TYPE: &str = "Entity";
const ENTITY_FIELD: &str = "bits";
const ANONYMOUS_FIELD: &str = "_: ";

#[derive(Debug)]
pub struct TextError;

impl Display for TextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("payload could not be written as text")
    }
}

impl std::error::Error for TextError {}

impl ser::Error for TextError {
    fn custom<T: Display>(_: T) -> Self {
        TextError
    }
}

#[derive(Debug, Default)]
pub struct PayloadText {
    pub text: String,
    pub entities: Vec<u64>,
}

pub fn describe<S: Serialize>(payload: &S) -> PayloadText {
    let mut writer = PayloadText::default();
    let _ = payload.serialize(&mut writer);
    if writer.text.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !writer.text.is_char_boundary(end) {
            end -= 1;
        }
        writer.text.truncate(end);
        writer.text.push_str("...");
    }
    writer
}

pub struct Items<'a> {
    out: &'a mut PayloadText,
    first: bool,
    close: &'static str,
    entity_at: Option<usize>,
    anonymous: bool,
}

impl Items<'_> {
    fn separate(&mut self) {
        if !self.first {
            self.out.text.push_str(", ");
        }
        self.first = false;
    }

    fn item<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.separate();
        if self.anonymous {
            self.out.text.push_str(ANONYMOUS_FIELD);
        }
        value.serialize(&mut *self.out)
    }

    fn named<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), TextError> {
        if self.entity_at.is_some() {
            return value.serialize(&mut *self.out);
        }
        self.separate();
        self.out.text.push_str(name);
        self.out.text.push_str(": ");
        value.serialize(&mut *self.out)
    }

    fn finish(self) -> Result<(), TextError> {
        let Some(start) = self.entity_at else {
            self.out.text.push_str(self.close);
            return Ok(());
        };
        let digits = self.out.text.split_off(start);
        match digits.parse::<u64>() {
            Ok(bits) => {
                self.out.entities.push(bits);
                let (index, generation) = (bits as u32, (bits >> 32) as u32);
                let _ = write!(self.out.text, "{ENTITY_TYPE}({index}v{generation})");
            }
            Err(_) => {
                let _ = write!(
                    self.out.text,
                    "{ENTITY_TYPE} {{ {ENTITY_FIELD}: {digits} }}"
                );
            }
        }
        Ok(())
    }
}

impl<'a> PayloadText {
    fn items(&'a mut self, open: &str, close: &'static str) -> Items<'a> {
        self.text.push_str(open);
        Items {
            out: self,
            first: true,
            close,
            entity_at: None,
            anonymous: false,
        }
    }

    fn named_items(&'a mut self, name: &str, close: &'static str, open: &str) -> Items<'a> {
        self.text.push_str(name);
        self.items(open, close)
    }

    fn anonymous_items(&'a mut self, name: &str) -> Items<'a> {
        let mut items = self.named_items(name, " }", " { ");
        items.anonymous = true;
        items
    }

    fn one_anonymous<T: Serialize + ?Sized>(
        &mut self,
        name: &str,
        value: &T,
    ) -> Result<(), TextError> {
        self.text.push_str(name);
        self.text.push_str(" { ");
        self.text.push_str(ANONYMOUS_FIELD);
        value.serialize(&mut *self)?;
        self.text.push_str(" }");
        Ok(())
    }

    fn shown(&mut self, value: impl Display) -> Result<(), TextError> {
        let _ = write!(self.text, "{value}");
        Ok(())
    }

    fn debugged(&mut self, value: impl fmt::Debug) -> Result<(), TextError> {
        let _ = write!(self.text, "{value:?}");
        Ok(())
    }

    fn wrapped<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<(), TextError> {
        self.text.push_str(name);
        self.text.push('(');
        value.serialize(&mut *self)?;
        self.text.push(')');
        Ok(())
    }
}

impl<'a> ser::Serializer for &'a mut PayloadText {
    type Ok = ();
    type Error = TextError;
    type SerializeSeq = Items<'a>;
    type SerializeTuple = Items<'a>;
    type SerializeTupleStruct = Items<'a>;
    type SerializeTupleVariant = Items<'a>;
    type SerializeMap = Items<'a>;
    type SerializeStruct = Items<'a>;
    type SerializeStructVariant = Items<'a>;

    fn serialize_bool(self, value: bool) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_i8(self, value: i8) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_i16(self, value: i16) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_i32(self, value: i32) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_i64(self, value: i64) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_i128(self, value: i128) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_u8(self, value: u8) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_u16(self, value: u16) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_u32(self, value: u32) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_u64(self, value: u64) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_u128(self, value: u128) -> Result<(), TextError> {
        self.shown(value)
    }
    fn serialize_f32(self, value: f32) -> Result<(), TextError> {
        self.debugged(value)
    }
    fn serialize_f64(self, value: f64) -> Result<(), TextError> {
        self.debugged(value)
    }
    fn serialize_char(self, value: char) -> Result<(), TextError> {
        self.debugged(value)
    }
    fn serialize_str(self, value: &str) -> Result<(), TextError> {
        self.debugged(value)
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<(), TextError> {
        self.debugged(value)
    }
    fn serialize_none(self) -> Result<(), TextError> {
        self.shown("None")
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), TextError> {
        self.wrapped("Some", value)
    }
    fn serialize_unit(self) -> Result<(), TextError> {
        self.shown("()")
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), TextError> {
        self.shown(name)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), TextError> {
        self.shown(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), TextError> {
        self.one_anonymous(name, value)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), TextError> {
        self.one_anonymous(variant, value)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Items<'a>, TextError> {
        Ok(self.items("[", "]"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Items<'a>, TextError> {
        Ok(self.items("(", ")"))
    }
    fn serialize_tuple_struct(self, name: &'static str, _: usize) -> Result<Items<'a>, TextError> {
        Ok(self.anonymous_items(name))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Items<'a>, TextError> {
        Ok(self.anonymous_items(variant))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Items<'a>, TextError> {
        Ok(self.items("{", "}"))
    }
    fn serialize_struct(self, name: &'static str, len: usize) -> Result<Items<'a>, TextError> {
        if name == ENTITY_TYPE && len == 1 {
            let start = self.text.len();
            let mut items = self.items("", "");
            items.entity_at = Some(start);
            return Ok(items);
        }
        Ok(self.named_items(name, " }", " { "))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Items<'a>, TextError> {
        Ok(self.named_items(variant, " }", " { "))
    }
}

impl ser::SerializeSeq for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.item(value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeTuple for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.item(value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.item(value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.item(value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeMap for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), TextError> {
        self.item(key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), TextError> {
        self.out.text.push_str(": ");
        value.serialize(&mut *self.out)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeStruct for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), TextError> {
        self.named(name, value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Items<'_> {
    type Ok = ();
    type Error = TextError;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), TextError> {
        self.named(name, value)
    }
    fn end(self) -> Result<(), TextError> {
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::describe;
    use crate::entity::Entity;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Hit {
        target: Entity,
        amount: u32,
        ratio: f32,
    }

    #[derive(Serialize)]
    struct Pair(u64, bool);

    #[derive(Serialize)]
    struct Single(u64);

    #[derive(Serialize)]
    struct Marker;

    #[derive(Serialize)]
    enum Order {
        Stop,
        Move(u32),
        Line(u32, u32),
        Aim { x: i32 },
    }

    #[derive(Serialize)]
    struct Holder {
        best: Option<u32>,
        none: Option<u32>,
        grid: [u8; 2],
    }

    #[test]
    fn a_struct_shows_its_fields_and_entities() {
        let hit = Hit {
            target: Entity::new(12, 3),
            amount: 5,
            ratio: 1.0,
        };
        let shown = describe(&hit);
        assert_eq!(
            shown.text,
            "Hit { target: Entity(12v3), amount: 5, ratio: 1.0 }"
        );
        assert_eq!(shown.entities, [Entity::new(12, 3).to_bits()]);
    }

    #[test]
    fn tuple_unit_and_newtype_structs_read_like_rust() {
        assert_eq!(describe(&Pair(7, true)).text, "Pair { _: 7, _: true }");
        assert_eq!(describe(&Single(7)).text, "Single { _: 7 }");
        assert_eq!(describe(&Marker).text, "Marker");
    }

    #[test]
    fn enums_show_their_variant() {
        assert_eq!(describe(&Order::Stop).text, "Stop");
        assert_eq!(describe(&Order::Move(4)).text, "Move { _: 4 }");
        assert_eq!(describe(&Order::Line(1, 2)).text, "Line { _: 1, _: 2 }");
        assert_eq!(describe(&Order::Aim { x: -3 }).text, "Aim { x: -3 }");
    }

    #[test]
    fn options_and_arrays_are_written_out() {
        let holder = Holder {
            best: Some(1),
            none: None,
            grid: [4, 5],
        };
        assert_eq!(
            describe(&holder).text,
            "Holder { best: Some(1), none: None, grid: (4, 5) }"
        );
    }

    #[test]
    fn very_long_payloads_are_cut() {
        let text = describe(&[1_234_567_890u64; 32]).text;
        assert!(text.len() <= super::MAX_TEXT_BYTES + 3);
        assert!(text.ends_with("..."));
    }
}
