/*
 * Copyright (c) 2020-2021 Rob Patro, Avi Srivastava, Hirak Sarkar, Dongze He, Mohsen Zakeri.
 *
 * This file is part of alevin-fry
 * (see https://github.com/COMBINE-lab/alevin-fry).
 *
 * License: 3-clause BSD, see https://opensource.org/licenses/BSD-3-Clause
 */

extern crate ahash;

use bstr::io::BufReadExt;
use needletail::bitkmer::*;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs::File;
use std::io::BufReader;

pub(super) const MASK_TOP_BIT_U32: u32 = 0x7FFFFFFF;
pub(super) const MASK_LOWER_31_U32: u32 = 0x80000000;

#[allow(dead_code)]
#[derive(Debug)]
pub(super) struct InternalVersionInfo {
    major: u32,
    minor: u32,
    patch: u32,
}

impl InternalVersionInfo {
    pub(super) fn from_str(vs: &str) -> Self {
        let versions: Vec<u32> = vs.split('.').map(|s| s.parse::<u32>().unwrap()).collect();
        assert_eq!(
            versions.len(),
            3,
            "The version string should be of the format x.y.z; it was {}",
            vs
        );
        Self {
            major: versions[0],
            minor: versions[1],
            patch: versions[2],
        }
    }

    pub(super) fn is_compatible_with(&self, other: &InternalVersionInfo) -> Result<(), String> {
        if self.major == other.major && self.minor == other.minor {
            Ok(())
        } else {
            let s = format!(
                "version {:?} is incompatible with version {:?}",
                self, other
            );
            Err(s)
        }
    }
}

pub fn is_velo_mode(input_dir: String) -> bool {
    let parent = std::path::Path::new(&input_dir);
    // open the metadata file and read the json
    let meta_data_file = File::open(parent.join("generate_permit_list.json"))
        .expect("could not open the generate_permit_list.json file.");
    let mdata: serde_json::Value = serde_json::from_reader(meta_data_file)
        .expect("could not deseralize generate_permit_list.json");
    let vm = mdata.get("velo_mode");
    match vm {
        Some(v) => v.as_bool().unwrap_or(false),
        None => false,
    }
}

/// FROM https://github.com/10XGenomics/rust-debruijn/blob/master/src/dna_string.rs
/// count Hamming distance between 2 2-bit DNA packed u64s
pub(super) fn count_diff_2_bit_packed(a: u64, b: u64) -> usize {
    let bit_diffs = a ^ b;
    let two_bit_diffs = (bit_diffs | bit_diffs >> 1) & 0x5555555555555555;
    two_bit_diffs.count_ones() as usize
}

fn get_bit_mask(nt_index: usize, fill_with: u64) -> u64 {
    let mut mask: u64 = fill_with;
    mask <<= 2 * (nt_index - 1);
    mask
}

fn get_all_snps(bc: u64, bc_length: usize) -> Vec<u64> {
    assert!(
        bc <= 2u64.pow(2 * bc_length as u32),
        "the barcode id is larger than possible (based on barcode length)"
    );
    assert!(
        bc_length <= 32,
        "barcode length greater than 32 not supported"
    );

    let mut snps: Vec<u64> = Vec::new();
    snps.reserve(3 * bc_length);

    for nt_index in 1..=bc_length {
        // clearing the two relevant bits based on nucleotide position
        let bit_mask = bc & !get_bit_mask(nt_index, 3);

        // iterating over all 4 choices of the nucleotide
        for i in 0..=3 {
            let new_bc = bit_mask | get_bit_mask(nt_index, i);
            if new_bc != bc {
                snps.push(new_bc);
            }
        }
    }

    snps
}

fn get_all_indels(bc: u64, bc_length: usize) -> Vec<u64> {
    assert!(
        bc <= 2u64.pow(2 * bc_length as u32),
        "the barcode id is larger than possible (based on barcode length)"
    );
    assert!(
        bc_length <= 32,
        "barcode length greater than 32 not supported"
    );

    let mut indels: Vec<u64> = Vec::new();
    indels.reserve(8 * (bc_length - 1));

    for nt_index in 1..bc_length {
        let mut bit_mask = 1 << (2 * nt_index);
        bit_mask -= 1;

        let upper_half = bc & !bit_mask;
        let lower_half = bc & bit_mask;

        // iterating over all 4 choices of the nucleotide
        for i in 0..=3 {
            let new_insertion_bc = upper_half | get_bit_mask(nt_index, i) | (lower_half >> 2);
            let new_deletion_bc = upper_half
                | get_bit_mask(1, i)
                | ((lower_half & !get_bit_mask(nt_index + 1, 3)) << 2);

            if new_insertion_bc != bc {
                indels.push(new_insertion_bc);
            }
            if new_deletion_bc != bc {
                indels.push(new_deletion_bc);
            }
        }
    }

    indels
}

pub fn get_all_one_edit_neighbors(
    bc: u64,
    bc_length: usize,
    neighbors: &mut HashSet<u64>,
) -> Result<(), Box<dyn Error>> {
    neighbors.clear();

    let snps: Vec<u64> = get_all_snps(bc, bc_length);
    let indels: Vec<u64> = get_all_indels(bc, bc_length);

    neighbors.extend(&snps);
    neighbors.extend(&indels);

    Ok(())
}

pub fn generate_whitelist_set(
    whitelist_bcs: &[u64],
    bc_length: usize,
) -> Result<HashSet<u64>, Box<dyn Error>> {
    let num_bcs = whitelist_bcs.len();

    let mut one_edit_barcode_hash: HashSet<u64> = HashSet::new();
    let mut neighbors: HashSet<u64> = HashSet::new();
    one_edit_barcode_hash.reserve(10 * num_bcs);
    // reserved space for 3*length SNP
    // + 4 * (length -1) insertion
    // + 4 * (length -1) deletion
    neighbors.reserve(3 * bc_length + 8 * (bc_length - 1));

    for bc in whitelist_bcs {
        get_all_one_edit_neighbors(*bc, bc_length, &mut neighbors)?;
        one_edit_barcode_hash.extend(&neighbors);
    }

    Ok(one_edit_barcode_hash)
}

/**
 * generates a map that contains all one edit distance neighbors
 * of the permitted barcodes.  The key is the neighbor and the value
 * is the original permitted barcode to which it maps.
 **/
pub fn generate_permitlist_map(
    permit_bcs: &[u64],
    bc_length: usize,
) -> Result<HashMap<u64, u64>, Box<dyn Error>> {
    let num_bcs = permit_bcs.len();

    let mut one_edit_barcode_map: HashMap<u64, u64> = HashMap::with_capacity(10 * num_bcs);
    // first insert everything already in the explicit permitlist
    for bc in permit_bcs {
        one_edit_barcode_map.insert(*bc, *bc);
    }

    // reserved space for 3*length SNP
    // + 4 * (length -1) insertion
    // + 4 * (length -1) deletion
    let mut neighbors: HashSet<u64> = HashSet::with_capacity(3 * bc_length + 8 * (bc_length - 1));

    for bc in permit_bcs {
        get_all_one_edit_neighbors(*bc, bc_length, &mut neighbors)?;
        for n in &neighbors {
            one_edit_barcode_map.entry(*n).or_insert(*bc);
        }
    }

    Ok(one_edit_barcode_map)
}

/// Reads the contents of the file `flist`, which should contain
/// a single barcode per-line, and returns a Result that is either
/// a HashSet containing the k-mer encoding of all barcodes or
/// the Error that was encountered parsing the file.
pub fn read_filter_list(
    flist: &str,
    bclen: u16,
) -> Result<HashSet<u64, ahash::RandomState>, Box<dyn std::error::Error>> {
    let s = ahash::RandomState::with_seeds(2u64, 7u64, 1u64, 8u64);
    let mut fset = HashSet::<u64, ahash::RandomState>::with_hasher(s);

    let filt_file = std::fs::File::open(flist).expect("couldn't open file");
    let reader = BufReader::new(filt_file);

    // Read the file line by line using the lines() iterator from std::io::BufRead.
    reader
        .for_byte_line(|line| {
            let mut bnk = BitNuclKmer::new(line, bclen as u8, false);
            let (_, k, _) = bnk.next().expect("can't extract kmer");
            fset.insert(k.0);
            Ok(true)
        })
        .unwrap();

    Ok(fset)
}

#[inline(always)]
fn unspliced_of(gid: u32) -> u32 {
    gid + 1
}

/// should always compile to no-op
#[inline(always)]
fn spliced_of(gid: u32) -> u32 {
    gid
}

/// Parse a 3 column tsv of the format
/// transcript_name gene_name   status
/// where status is one of S or U each gene will be allocated both a spliced and
/// unspliced variant, the spliced index will always be even and the unspliced odd,
/// and they will always be adjacent ids.  For example, if gene A is present in
/// the sample and it's spliced variant is assigned id i,  then it will always be true that
/// i % 2 == 0
/// and
/// (i+1) will be the id for the unspliced version of gene A
fn parse_tg_spliced_unspliced(
    rdr: &mut csv::Reader<File>,
    ref_count: usize,
    rname_to_id: &HashMap<String, u32, ahash::RandomState>,
    gene_names: &mut Vec<String>,
    gene_name_to_id: &mut HashMap<String, u32, ahash::RandomState>,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    // map each transcript id to the corresponding gene id
    // the transcript name can be looked up from the id in the RAD header,
    // and the gene name can be looked up from the id in the gene_names
    // vector.
    let mut tid_to_gid = vec![u32::MAX; ref_count];

    // Record will be transcript, gene, splicing status
    type TsvRec = (String, String, String);

    // the transcripts for which we've found a gene mapping
    let mut found = 0usize;

    // starting from 0, we assign each gene 2 ids (2 consecutive integers),
    // the even ids are for spliced txps, the odd ids are for unspliced txps
    // for convenience, we define a gid helper, next_gid
    let mut next_gid = 0u32;
    // apparently the "header" (first row) will be included
    // in the iterator returned by `deserialize` anyway
    /*let hdr = rdr.headers()?;
    let hdr_vec : Vec<Result<TsvRec,csv::Error>> = vec![hdr.deserialize(None)];
    */
    for result in rdr.deserialize() {
        let record: TsvRec = result?;
        // first, get the first id for this gene
        let gene_id = *gene_name_to_id.entry(record.1.clone()).or_insert_with(|| {
            // as we need to return the current next_gid if we run this code
            // we add by two and then return current gene id.
            let cur_gid = next_gid;
            next_gid += 2;
            // we haven't added this gene name already,
            // we append it now to the list of gene names.
            gene_names.push(record.1.clone());
            cur_gid
        });

        // get the transcript id
        if let Some(transcript_id) = rname_to_id.get(&record.0) {
            found += 1;
            if record.2.eq_ignore_ascii_case("U") {
                // This is an unspliced txp
                // we link it to the second gid of this gene
                tid_to_gid[*transcript_id as usize] = unspliced_of(gene_id);
            } else if record.2.eq_ignore_ascii_case("S") {
                // This is a spliced txp, we link it to the
                // first gid of this gene
                tid_to_gid[*transcript_id as usize] = spliced_of(gene_id);
            } else {
                return Err("Third column in 3 column txp-to-gene file must be S or U".into());
            }
        }
    }

    assert_eq!(
        found, ref_count,
        "The tg-map must contain a gene mapping for all transcripts in the header"
    );

    Ok(tid_to_gid)
}

fn parse_tg_spliced(
    rdr: &mut csv::Reader<File>,
    ref_count: usize,
    rname_to_id: &HashMap<String, u32, ahash::RandomState>,
    gene_names: &mut Vec<String>,
    gene_name_to_id: &mut HashMap<String, u32, ahash::RandomState>,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    // map each transcript id to the corresponding gene id
    // the transcript name can be looked up from the id in the RAD header,
    // and the gene name can be looked up from the id in the gene_names
    // vector.
    let mut tid_to_gid = vec![u32::MAX; ref_count];
    // now read in the transcript to gene map
    type TsvRec = (String, String);
    // now, map each transcript index to it's corresponding gene index
    let mut found = 0usize;
    // apparently the "header" (first row) will be included
    // in the iterator returned by `deserialize` anyway
    /*let hdr = rdr.headers()?;
    let hdr_vec : Vec<Result<TsvRec,csv::Error>> = vec![hdr.deserialize(None)];
    */
    for result in rdr.deserialize() {
        match result {
            Ok(record_in) => {
                let record: TsvRec = record_in;
                //let record: TSVRec = result?;
                // first, get the id for this gene
                let next_id = gene_name_to_id.len() as u32;
                let gene_id = *gene_name_to_id.entry(record.1.clone()).or_insert(next_id);
                // if we haven't added this gene name already, then
                // append it now to the list of gene names.
                if gene_id == next_id {
                    gene_names.push(record.1.clone());
                }
                // get the transcript id
                if let Some(transcript_id) = rname_to_id.get(&record.0) {
                    found += 1;
                    tid_to_gid[*transcript_id as usize] = gene_id;
                }
            }
            Err(e) => {
                /*
                crit!(
                    log,
                    "Encountered error [{}] when reading the transcript-to-gene map. Please make sure the transcript-to-gene mapping is a 2 column, tab separated file.",
                    e
                );
                */
                return Err(Box::new(e));
            }
        }
    }

    assert_eq!(
        found, ref_count,
        "The tg-map must contain a gene mapping for all transcripts in the header"
    );

    Ok(tid_to_gid)
}

pub fn parse_tg_map(
    tg_map: &str,
    ref_count: usize,
    rname_to_id: &HashMap<String, u32, ahash::RandomState>,
    gene_names: &mut Vec<String>,
    gene_name_to_id: &mut HashMap<String, u32, ahash::RandomState>,
) -> Result<Vec<u32>, Box<dyn std::error::Error>> {
    let t2g_file = std::fs::File::open(tg_map).expect("couldn't open file");
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .delimiter(b'\t')
        .from_reader(t2g_file);

    let headers = rdr.headers()?;
    match headers.len() {
        2 => {
            // parse the 2 column format
            parse_tg_spliced(
                &mut rdr,
                ref_count,
                rname_to_id,
                gene_names,
                gene_name_to_id,
            )
        }
        3 => {
            // parse the 3 column format
            parse_tg_spliced_unspliced(
                &mut rdr,
                ref_count,
                rname_to_id,
                gene_names,
                gene_name_to_id,
            )
        }
        _ => {
            // not supported
            Err("Transcript-gene mapping must have either 2 or 3 columns.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use self::libradicl::utils::*;
    use crate as libradicl;
    use std::collections::HashSet;

    #[test]
    fn test_get_bit_mask() {
        let mut output = Vec::new();
        for i in 0..=3 {
            let mask = get_bit_mask(2, i);
            output.push(mask);
        }
        assert_eq!(output, vec![0, 4, 8, 12]);
    }

    #[test]
    fn test_get_all_snps() {
        let mut output: Vec<u64> = get_all_snps(7, 3).into_iter().collect();
        output.sort();

        assert_eq!(output, vec![3, 4, 5, 6, 11, 15, 23, 39, 55]);
    }

    #[test]
    fn test_get_all_indels() {
        let mut output: Vec<u64> = get_all_indels(7, 3).into_iter().collect();
        output.sort();
        output.dedup();

        assert_eq!(output, vec![1, 4, 5, 6, 9, 12, 13, 14, 15, 28, 29, 30, 31]);
    }

    #[test]
    fn test_get_all_one_edit_neighbors() {
        let mut neighbors: HashSet<u64> = HashSet::new();
        get_all_one_edit_neighbors(7, 3, &mut neighbors).unwrap();

        let mut output: Vec<u64> = neighbors.into_iter().collect();

        output.sort();
        output.dedup();

        assert_eq!(
            output,
            vec![1, 3, 4, 5, 6, 9, 11, 12, 13, 14, 15, 23, 28, 29, 30, 31, 39, 55]
        );
    }

    #[test]
    fn test_generate_whitelist_hash() {
        let neighbors: HashSet<u64> = generate_whitelist_set(&vec![7], 3).unwrap();
        let mut output: Vec<u64> = neighbors.into_iter().collect();

        output.sort();
        output.dedup();

        assert_eq!(
            output,
            vec![1, 3, 4, 5, 6, 9, 11, 12, 13, 14, 15, 23, 28, 29, 30, 31, 39, 55]
        );
    }

    #[test]
    fn test_version_info() {
        let vi = InternalVersionInfo::from_str("1.2.3");
        assert_eq!(
            vi,
            InternalVersionInfo{1, 2, 3}
        );
    }
}
