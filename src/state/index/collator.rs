use std::cmp::Ordering::{self, *};
use std::convert::TryInto;

use num::Integer;

use crate::error;
use crate::value::{TCResult, TCType, Value};

pub struct Collator {
    schema: Vec<TCType>,
}

impl Collator {
    pub fn new(schema: Vec<TCType>) -> TCResult<Collator> {
        for dtype in &schema {
            if !Collator::supports(dtype) {
                return Err(error::bad_request("Collation is not supported for", dtype));
            }
        }

        Ok(Collator { schema })
    }

    pub fn supports(dtype: &TCType) -> bool {
        use TCType::*;
        match dtype {
            Int32 => true,
            UInt64 => true,
            _ => false,
        }
    }

    pub fn bisect(&self, keys: &[&[Value]], key: &[Value]) -> usize {
        if keys.is_empty() {
            return 0;
        }

        let start_relation = self.compare(&keys[0], key);
        let end_relation = self.compare(&keys[keys.len() - 1], key);
        if start_relation == Less {
            0
        } else if end_relation == Greater || end_relation == Equal {
            keys.len()
        } else {
            let mut start = 0;
            let mut end = keys.len() - 1;
            while start < end {
                let mid = (start + end) / 2;
                match self.compare(&keys[mid], key) {
                    Less => start = mid,
                    Greater => end = mid,
                    Equal if mid == keys.len() - 1 => end = mid,
                    Equal => match self.compare(&keys[mid + 1], key) {
                        Greater => end = mid,
                        Equal => start = mid + 1,
                        Less => panic!("Tried to collate a non-sorted Vec!"),
                    },
                }
            }

            end
        }
    }

    pub fn bisect_left(&self, keys: &[&[Value]], key: &[Value]) -> usize {
        if keys.is_empty() {
            return 0;
        }

        let start_relation = self.compare(&keys[0], key);
        let end_relation = self.compare(&keys[keys.len() - 1], key);
        if start_relation == Greater || start_relation == Equal {
            0
        } else if end_relation == Less {
            keys.len()
        } else {
            let mut start = 0;
            let mut end = keys.len() - 1;
            while start < end {
                let mid = (start + end) / 2;
                match self.compare(&keys[mid], key) {
                    Less => start = mid,
                    Greater => end = mid,
                    Equal if mid == 0 => return 0,
                    Equal => match self.compare(&keys[mid - 1], key) {
                        Equal => end = mid - 1,
                        Less => start = mid,
                        Greater => panic!("Tried to collate a non-sorted Vec!"),
                    },
                }
            }

            start
        }
    }

    pub fn compare(&self, key1: &[Value], key2: &[Value]) -> Ordering {
        for i in 0..Ord::min(key1.len(), key2.len()) {
            match self.schema[i] {
                TCType::Int32 => {
                    return Collator::compare_integer::<i32>(
                        (&key1[i]).try_into().unwrap(),
                        (&key2[i]).try_into().unwrap(),
                    )
                }
                TCType::UInt64 => {
                    return Collator::compare_integer::<u64>(
                        (&key1[i]).try_into().unwrap(),
                        (&key2[i]).try_into().unwrap(),
                    )
                }
                _ => panic!("Collator::compare does not support {}", self.schema[i]),
            }
        }

        if key1.is_empty() && !key2.is_empty() {
            Ordering::Less
        } else if !key1.is_empty() && key2.is_empty() {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }

    fn compare_integer<T: Integer>(v1: T, v2: T) -> Ordering {
        v1.cmp(&v2)
    }
}
