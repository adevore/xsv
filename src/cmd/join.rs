use std::collections::hashmap::{HashMap, Vacant, Occupied};
// use collections::btree::{BTreeMap, Vacant, Occupied}; 
use std::fmt;
use std::io;

use csv::{mod, ByteString};
use csv::index::Indexed;
use docopt;

use types::{
    CliError, CsvConfig, Delimiter, NormalSelection, Selection, SelectColumns
};
use util;

docopt!(Args, "
Joins two sets of CSV data on the specified columns.

The default join operation is an 'inner' join. This corresponds to the
intersection of rows on the keys specified.

The columns arguments specify the columns to join for each input. Columns can
be referenced by name or index, starting at 1. Specify multiple columns by
separating them with a comma. Specify a range of columns with `-`. Both
columns1 and columns2 must specify exactly the same number of columns.

Usage:
    xsv join [options] <columns1> <input1> <columns2> <input2>
    xsv join --help

join options:
    --left                 Do a 'left outer' join. This returns all rows in
                           first CSV data set, including rows with no
                           corresponding row in the second data set. When no
                           corresponding row exists, it is padded out with
                           empty fields.
    --right                Do a 'right outer' join. This returns all rows in
                           second CSV data set, including rows with no
                           corresponding row in the first data set. When no
                           corresponding row exists, it is padded out with
                           empty fields. (This is the reverse of 'outer left'.)
    --full                 Do a 'full outer' join. This returns all rows in
                           both data sets with matching records joined. If
                           there is no match, the missing side will be padded
                           out with empty fields. (This is the combination of
                           'outer left' and 'outer right'.)
    --cross                USE WITH CAUTION.
                           This returns the cartesian product of the CSV
                           data sets given. The number of rows return is
                           equal to N * M, where N and M correspond to the
                           number of rows in the given data sets, respectively.

Common options:
    -h, --help             Display this message
    -o, --output <file>    Write output to <file> instead of stdout.
    -n, --no-headers       When set, the first row will not be interpreted
                           as headers. (i.e., They are not searched, analyzed,
                           sliced, etc.)
    -d, --delimiter <arg>  The field delimiter for reading CSV data.
                           Must be a single character. [default: ,]
", arg_columns1: SelectColumns, arg_input1: String,
   arg_columns2: SelectColumns, arg_input2: String,
   flag_output: Option<String>, flag_delimiter: Delimiter,
   flag_left: bool, flag_right: bool, flag_full: bool, flag_cross: bool)

pub fn main() -> Result<(), CliError> {
    let args: Args = try!(util::get_args());
    let mut state = try!(args.new_io_state());
    match (
        args.flag_left,
        args.flag_right,
        args.flag_full,
        args.flag_cross,
    ) {
        (true, false, false, false) => {
            try!(state.write_headers());
            state.outer_join(false)
        }
        (false, true, false, false) => {
            try!(state.write_headers());
            state.outer_join(true)
        }
        (false, false, true, false) => {
            try!(state.write_headers());
            state.full_outer_join()
        }
        (false, false, false, true) => {
            try!(state.write_headers());
            state.cross_join()
        }
        (false, false, false, false) => {
            try!(state.write_headers());
            state.inner_join()
        }
        _ => Err(CliError::from_str("Please pick exactly one join operation."))
    }
}

struct IoState<R, W> {
    wtr: csv::Writer<W>,
    rdr1: csv::Reader<R>,
    sel1: Selection,
    rdr2: csv::Reader<R>,
    sel2: Selection,
    no_headers: bool,
}

impl<R: io::Reader + io::Seek, W: io::Writer> IoState<R, W> {
    fn write_headers(&mut self) -> Result<(), CliError> {
        let headers1 = try!(csv| self.rdr1.byte_headers());
        let headers2 = try!(csv| self.rdr2.byte_headers());
        if !self.no_headers {
            let mut headers = headers1.clone();
            headers.push_all(headers2[]);
            try!(csv| self.wtr.write_bytes(headers.into_iter()));
        }
        Ok(())
    }

    fn inner_join(mut self) -> Result<(), CliError> {
        let mut validx = try!(ValueIndex::new(self.rdr2, &self.sel2.normal()));
        for row in self.rdr1.byte_records() {
            let row = try!(csv| row);
            let val = self.sel1.select(row[])
                               .map(ByteString::from_bytes)
                               .collect::<Vec<ByteString>>();
            match validx.values.find(&val) {
                None => continue,
                Some(rows) => {
                    for &rowi in rows.iter() {
                        try!(csv| validx.idx.seek(rowi));

                        let mut row1 = row.iter().map(|f| Ok(f.as_slice()));
                        let row2 = validx.idx.csv().by_ref();
                        let combined = row1.by_ref().chain(row2);
                        try!(csv| self.wtr.write_results(combined));
                    }
                }
            }
        }
        Ok(())
    }

    fn outer_join(mut self, right: bool) -> Result<(), CliError> {
        if right {
            ::std::mem::swap(&mut self.rdr1, &mut self.rdr2);
            ::std::mem::swap(&mut self.sel1, &mut self.sel2);
        }

        let (_, pad2) = try!(self.get_padding());
        let mut validx = try!(ValueIndex::new(self.rdr2, &self.sel2.normal()));
        for row in self.rdr1.byte_records() {
            let row = try!(csv| row);
            let val = self.sel1.select(row[])
                               .map(ByteString::from_bytes)
                               .collect::<Vec<ByteString>>();
            match validx.values.find(&val) {
                None => {
                    let row1 = row.iter().map(|f| Ok(f[]));
                    let row2 = pad2.iter().map(|f| Ok(f[]));
                    if right {
                        try!(csv| self.wtr.write_results(row2.chain(row1)));
                    } else {
                        try!(csv| self.wtr.write_results(row1.chain(row2)));
                    }
                }
                Some(rows) => {
                    for &rowi in rows.iter() {
                        try!(csv| validx.idx.seek(rowi));
                        let row1 = row.iter().map(|f| Ok(f.as_slice()));
                        let row2 = validx.idx.csv().by_ref();
                        if right {
                            try!(csv| self.wtr.write_results(row2.chain(row1)));
                        } else {
                            try!(csv| self.wtr.write_results(row1.chain(row2)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn full_outer_join(mut self) -> Result<(), CliError> {
        let (pad1, pad2) = try!(self.get_padding());
        let mut validx = try!(ValueIndex::new(self.rdr2, &self.sel2.normal()));

        // Keep track of which rows we've written from rdr2.
        let mut rdr2_written = Vec::from_elem(validx.num_rows as uint, false);
        for row1 in self.rdr1.byte_records() {
            let row1 = try!(csv| row1);

            let val = self.sel1.select(row1[])
                               .map(ByteString::from_bytes)
                               .collect::<Vec<ByteString>>();
            match validx.values.find(&val) {
                None => {
                    let row1 = row1.iter().map(|f| Ok(f[]));
                    let row2 = pad2.iter().map(|f| Ok(f[]));
                    try!(csv| self.wtr.write_results(row1.chain(row2)));
                }
                Some(rows) => {
                    for &rowi in rows.iter() {
                        *rdr2_written.get_mut(rowi as uint) = true;

                        try!(csv| validx.idx.seek(rowi));
                        let row1 = row1.iter().map(|f| Ok(f[]));
                        let row2 = validx.idx.csv().by_ref();
                        try!(csv| self.wtr.write_results(row1.chain(row2)));
                    }
                }
            }
        }

        // OK, now write any row from rdr2 that didn't get joined with a row
        // from rdr1.
        for (i, &written) in rdr2_written.iter().enumerate() {
            if !written {
                try!(csv| validx.idx.seek(i as u64));
                let row1 = pad1.iter().map(|f| Ok(f[]));
                let row2 = validx.idx.csv().by_ref();
                try!(csv| self.wtr.write_results(row1.chain(row2)));
            }
        }
        Ok(())
    }

    fn cross_join(mut self) -> Result<(), CliError> {
        for row1 in self.rdr1.byte_records() {
            let row1 = try!(csv| row1);

            try!(csv| self.rdr2.seek(0, io::SeekSet));
            let mut first = true;
            while !self.rdr2.done() {
                // Skip the header row. The raw byte interface won't
                // do it for us.
                if first {
                    for f in self.rdr2 { try!(csv| f); }
                    first = false;
                }
                let row1 = row1.iter().map(|f| Ok(f[]));
                let row2 = self.rdr2.by_ref();
                try!(csv| self.wtr.write_results(row1.chain(row2)));
            }
        }
        Ok(())
    }

    fn get_padding(&mut self)
        -> Result<(Vec<ByteString>, Vec<ByteString>), CliError> {
        let len1 = try!(csv| self.rdr1.byte_headers()).len();
        let len2 = try!(csv| self.rdr2.byte_headers()).len();
        let (nada1, nada2) = (util::empty_field(), util::empty_field());
        Ok((Vec::from_elem(len1, nada1), Vec::from_elem(len2, nada2)))
    }
}

impl Args {
    fn new_io_state(&self)
        -> Result<IoState<io::File, Box<io::Writer+'static>>, CliError> {
        let rconf1 = CsvConfig::new(Some(self.arg_input1.clone()))
                               .delimiter(self.flag_delimiter)
                               .no_headers(self.flag_no_headers);
        let rconf2 = CsvConfig::new(Some(self.arg_input2.clone()))
                               .delimiter(self.flag_delimiter)
                               .no_headers(self.flag_no_headers);

        let mut rdr1 = try!(io| rconf1.reader_file());
        let mut rdr2 = try!(io| rconf2.reader_file());
        let (sel1, sel2) = try!(self.get_selections(&rconf1, &mut rdr1,
                                                    &rconf2, &mut rdr2));
        Ok(IoState {
            wtr: try!(io| CsvConfig::new(self.flag_output.clone()).writer()),
            rdr1: rdr1,
            sel1: sel1,
            rdr2: rdr2,
            sel2: sel2,
            no_headers: self.flag_no_headers,
        })
    }

    fn get_selections<R: Reader>
                     (&self,
                      rconf1: &CsvConfig, rdr1: &mut csv::Reader<R>,
                      rconf2: &CsvConfig, rdr2: &mut csv::Reader<R>)
                     -> Result<(Selection, Selection), CliError> {
        let headers1 = try!(csv| rdr1.byte_headers());
        let headers2 = try!(csv| rdr2.byte_headers());
        let select1 =
            try!(str| self.arg_columns1.selection(rconf1, headers1[]));
        let select2 =
            try!(str| self.arg_columns2.selection(rconf2, headers2[]));
        if select1.len() != select2.len() {
            return Err(CliError::from_str(format!(
                "Column selections must have the same number of columns, \
                 but found column selections with {} and {} columns.",
                select1.len(), select2.len())));
        }
        Ok((select1, select2))
    }
}

struct ValueIndex<R> {
    // This maps tuples of values to corresponding rows.
    values: HashMap<Vec<ByteString>, Vec<u64>>,
    idx: Indexed<R, io::MemReader>,
    num_rows: u64,
}

impl<R: Reader + Seek> ValueIndex<R> {
    fn new(mut rdr: csv::Reader<R>, nsel: &NormalSelection)
          -> Result<ValueIndex<R>, CliError> {
        let mut val_idx = HashMap::with_capacity(10000);
        // let mut val_idx = BTreeMap::new(); 
        let mut rows = io::MemWriter::with_capacity(8 * 10000);
        let mut rowi = 0u64;
        try!(io| rows.write_be_u64(0)); // offset to the first row, which
                                        // has already been read as a header.
        while !rdr.done() {
            // This is a bit hokey. We're doing this manually instead of
            // calling `csv::index::create` so we can create both indexes
            // in one pass.
            try!(io| rows.write_be_u64(rdr.byte_offset()));

            let fields = try!(csv| nsel.select(unsafe { rdr.byte_fields() })
                                       .map(|v| v.map(ByteString::from_bytes))
                                       .collect::<Result<Vec<_>, _>>());
            match val_idx.entry(fields) {
                Vacant(v) => {
                    let mut rows = Vec::with_capacity(4);
                    rows.push(rowi);
                    v.set(rows);
                }
                Occupied(mut v) => { v.get_mut().push(rowi); }
            }
            rowi += 1;
        }
        Ok(ValueIndex {
            values: val_idx,
            idx: try!(csv| Indexed::new(rdr, io::MemReader::new(rows.unwrap()))),
            num_rows: rowi,
        })
    }
}

impl<R> fmt::Show for ValueIndex<R> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Sort the values by order of first appearance.
        let mut kvs = self.values.iter().collect::<Vec<_>>();
        kvs.sort_by(|&(_, v1), &(_, v2)| v1[0].cmp(&v2[0]));
        for (keys, rows) in kvs.into_iter() {
            // This is just for debugging, so assume Unicode for now.
            let keys = keys.iter()
                           .map(|k| String::from_utf8(k[].to_vec()).unwrap())
                           .collect::<Vec<_>>();
            try!(writeln!(f, "({}) => {}", keys.connect(", "), rows))
        }
        Ok(())
    }
}