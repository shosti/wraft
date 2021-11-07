pub struct RingBuf<T> {
    data: Vec<Option<(usize, T)>>,
    capacity: usize,
    min_idx: usize,
    next_idx: usize,
}

impl<T> RingBuf<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        let mut data = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            data.push(None);
        }

        Self {
            data,
            capacity,
            min_idx: 0,
            next_idx: 0,
        }
    }

    pub fn add(&mut self, value: T) -> Result<usize, Error<T>> {
        let idx = self.next_idx;
        if idx - self.min_idx >= self.capacity {
            return Err(Error::Overflow(value));
        }

        let i = self.data_idx(idx);
        self.data[i] = Some((idx, value));
        self.next_idx += 1;

        Ok(idx)
    }

    pub fn remove(&mut self, idx: usize) -> Option<T> {
        let i = self.data_idx(idx);
        let val = self.data.get(i).unwrap();
        if val.is_none() {
            return None;
        }
        if val.as_ref().unwrap().0 != idx {
            return None;
        }

        let entry = self.data.get_mut(i).unwrap();
        let (_, val) = entry.take().unwrap();
        while self.min_idx < self.next_idx && self.data[self.data_idx(self.min_idx)].is_none() {
            self.min_idx += 1;
        }

        Some(val)
    }

    fn data_idx(&self, idx: usize) -> usize {
        idx % self.capacity
    }
}

#[derive(Debug, PartialEq)]
pub enum Error<T> {
    Overflow(T),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overflow() {
        let mut buf = RingBuf::with_capacity(4);
        for i in 0..4 {
            let ret = buf.add("foo");
            assert_eq!(ret.unwrap(), i);
        }
        let ohno = buf.add("nope!");
        assert_eq!(ohno, Err(Error::Overflow("nope!")));

        assert_eq!(buf.remove(0).unwrap(), "foo");
        assert_eq!(Ok(4), buf.add("ok now!"));
    }

    #[test]
    fn test_add_variations() {
        let mut buf = RingBuf::with_capacity(4);
        let a_idx = buf.add("a").unwrap();
        let b_idx = buf.add("b").unwrap();
        let c_idx = buf.add("c").unwrap();
        let d_idx = buf.add("d").unwrap();

        assert_eq!(buf.remove(b_idx).unwrap(), "b");
        assert_eq!(buf.remove(c_idx).unwrap(), "c");
        assert_eq!(buf.remove(d_idx).unwrap(), "d");
        assert_eq!(buf.remove(a_idx).unwrap(), "a");
        assert_eq!(buf.remove(b_idx), None);

        let foo_idx = buf.add("foo").unwrap();
        assert_eq!(buf.remove(a_idx), None);
        assert_eq!(buf.remove(foo_idx).unwrap(), "foo");
    }
}
