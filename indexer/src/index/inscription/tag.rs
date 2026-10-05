use std::collections::BTreeMap;

#[derive(Copy, Clone)]
#[repr(u8)]
pub(crate) enum Tag {
    #[allow(unused)]
    Unbound = 66,

    ContentType = 1,
    ContentEncoding = 9,
    #[allow(unused)]
    Note = 15,
    #[allow(unused)]
    Nop = 255,
}

impl Tag {
    pub(crate) fn bytes(self) -> [u8; 1] {
        [self as u8]
    }

    pub(crate) fn take(self, fields: &mut BTreeMap<&[u8], Vec<&[u8]>>) -> Option<Vec<u8>> {
        let values = fields.get_mut(self.bytes().as_slice())?;

        if values.is_empty() {
            None
        } else {
            let value = values.remove(0).to_vec();

            if values.is_empty() {
                fields.remove(self.bytes().as_slice());
            }

            Some(value)
        }
    }
}
