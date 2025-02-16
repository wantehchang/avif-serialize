//! # AVIF image serializer (muxer)
//!
//! ## Usage
//!
//! 1. Compress pixels using an AV1 encoder, such as [rav1e](//lib.rs/rav1e). [libaom](//lib.rs/libaom-sys) works too.
//!
//! 2. Call `avif_serialize::serialize_to_vec(av1_data, None, width, height, 8)`
//!
//! See [cavif](https://github.com/kornelski/cavif-rs) for a complete implementation.

mod boxes;
mod writer;

use crate::boxes::*;
use arrayvec::ArrayVec;
use std::io;

/// Config for the serialization (allows setting advanced image properties).
///
/// See [`Aviffy::new`].
pub struct Aviffy {
    premultiplied_alpha: bool,
}

/// Makes an AVIF file given encoded AV1 data (create the data with [`rav1e`](//lib.rs/rav1e))
///
/// `color_av1_data` is already-encoded AV1 image data for the color channels (YUV, RGB, etc.).
/// The color image MUST have been encoded without chroma subsampling AKA YUV444 (`Cs444` in `rav1e`)
/// AV1 handles full-res color so effortlessly, you should never need chroma subsampling ever again.
///
/// Optional `alpha_av1_data` is a monochrome image (`rav1e` calls it "YUV400"/`Cs400`) representing transparency.
/// Alpha adds a lot of header bloat, so don't specify it unless it's necessary.
///
/// `width`/`height` is image size in pixels. It must of course match the size of encoded image data.
/// `depth_bits` should be 8, 10 or 12, depending on how the image was encoded (typically 8).
///
/// Color and alpha must have the same dimensions and depth.
///
/// Data is written (streamed) to `into_output`.
pub fn serialize<W: io::Write>(into_output: W, color_av1_data: &[u8], alpha_av1_data: Option<&[u8]>, width: u32, height: u32, depth_bits: u8) -> io::Result<()> {
    Aviffy::new().write(into_output, color_av1_data, alpha_av1_data, width, height, depth_bits)
}

impl Aviffy {
    pub fn new() -> Self {
        Self {
            premultiplied_alpha: false,
        }
    }

    /// Set whether image's colorspace uses premultiplied alpha, i.e. RGB channels were multiplied by their alpha value,
    /// so that transparent areas are all black. Image decoders will be instructed to undo the premultiplication.
    ///
    /// Premultiplied alpha images usually compress better and tolerate heavier compression, but
    /// may not be supported correctly by less capable AVIF decoders.
    ///
    /// This just sets the configuration property. The pixel data must have already been processed before compression.
    pub fn premultiplied_alpha(&mut self, is_premultiplied: bool) -> &mut Self {
        self.premultiplied_alpha = is_premultiplied;
        self
    }

    /// Makes an AVIF file given encoded AV1 data (create the data with [`rav1e`](//lib.rs/rav1e))
    ///
    /// `color_av1_data` is already-encoded AV1 image data for the color channels (YUV, RGB, etc.).
    /// The color image MUST have been encoded without chroma subsampling AKA YUV444 (`Cs444` in `rav1e`)
    /// AV1 handles full-res color so effortlessly, you should never need chroma subsampling ever again.
    ///
    /// Optional `alpha_av1_data` is a monochrome image (`rav1e` calls it "YUV400"/`Cs400`) representing transparency.
    /// Alpha adds a lot of header bloat, so don't specify it unless it's necessary.
    ///
    /// `width`/`height` is image size in pixels. It must of course match the size of encoded image data.
    /// `depth_bits` should be 8, 10 or 12, depending on how the image was encoded (typically 8).
    ///
    /// Color and alpha must have the same dimensions and depth.
    ///
    /// Data is written (streamed) to `into_output`.
    pub fn write<W: io::Write>(&self, into_output: W, color_av1_data: &[u8], alpha_av1_data: Option<&[u8]>, width: u32, height: u32, depth_bits: u8) -> io::Result<()> {
        let mut image_items = ArrayVec::new();
        let mut iloc_items = ArrayVec::new();
        let mut compatible_brands = ArrayVec::new();
        let mut ipma_entries = ArrayVec::new();
        let mut data_chunks = ArrayVec::<&[u8], 4>::new();
        let mut irefs = ArrayVec::new();
        let mut ipco = IpcoBox::new();
        let color_image_id = 1;
        let alpha_image_id = 2;
        let high_bitdepth = depth_bits >= 10;
        let twelve_bit = depth_bits >= 12;
        const ESSENTIAL_BIT: u8 = 0x80;

        image_items.push(InfeBox {
            id: color_image_id,
            typ: FourCC(*b"av01"),
            name: "",
        });
        let ispe_prop = ipco.push(IpcoProp::Ispe(IspeBox { width, height }));
        // This is redundant, but Chrome wants it, and checks that it matches :(
        let av1c_prop = ipco.push(IpcoProp::Av1C(Av1CBox {
            seq_profile: if twelve_bit { 2 } else { 1 },
            seq_level_idx_0: 31,
            seq_tier_0: false,
            high_bitdepth,
            twelve_bit,
            monochrome: false,
            chroma_subsampling_x: false,
            chroma_subsampling_y: false,
            chroma_sample_position: 0,
        }));
        // Useless bloat
        let pixi_3 = ipco.push(IpcoProp::Pixi(PixiBox {
            channels: 3,
            depth: 8,
        }));
        ipma_entries.push(IpmaEntry {
            item_id: color_image_id,
            prop_ids: [ispe_prop, av1c_prop | ESSENTIAL_BIT, pixi_3].iter().copied().collect(),
        });

        if let Some(alpha_data) = alpha_av1_data {
            image_items.push(InfeBox {
                id: alpha_image_id,
                typ: FourCC(*b"av01"),
                name: "",
            });
            let av1c_prop = ipco.push(boxes::IpcoProp::Av1C(Av1CBox {
                seq_profile: if twelve_bit { 2 } else { 0 },
                seq_level_idx_0: 31,
                seq_tier_0: false,
                high_bitdepth,
                twelve_bit,
                monochrome: true,
                chroma_subsampling_x: true,
                chroma_subsampling_y: true,
                chroma_sample_position: 0,
            }));
            // So pointless
            let pixi_1 = ipco.push(IpcoProp::Pixi(PixiBox {
                channels: 1,
                depth: 8,
            }));

            // that's a silly way to add 1 bit of information, isn't it?
            let auxc_prop = ipco.push(IpcoProp::AuxC(AuxCBox {
                urn: "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
            }));
            irefs.push(IrefBox {
                entry: IrefEntryBox {
                    from_id: alpha_image_id,
                    to_id: color_image_id,
                    typ: FourCC(*b"auxl"),
                },
            });
            if self.premultiplied_alpha {
                irefs.push(IrefBox {
                    entry: IrefEntryBox {
                        from_id: color_image_id,
                        to_id: alpha_image_id,
                        typ: FourCC(*b"prem"),
                    },
                });
            }
            ipma_entries.push(IpmaEntry {
                item_id: alpha_image_id,
                prop_ids: [ispe_prop, av1c_prop | ESSENTIAL_BIT, auxc_prop, pixi_1].iter().copied().collect(),
            });

            // Use interleaved color and alpha, with alpha first.
            // Makes it possible to display partial image.
            iloc_items.push(IlocItem {
                id: color_image_id,
                extents: [
                    IlocExtent {
                        offset: IlocOffset::Relative(alpha_data.len()),
                        len: color_av1_data.len(),
                    },
                ].into(),
            });
            iloc_items.push(IlocItem {
                id: alpha_image_id,
                extents: [
                    IlocExtent {
                        offset: IlocOffset::Relative(0),
                        len: alpha_data.len(),
                    },
                ].into(),
            });
            data_chunks.push(alpha_data);
            data_chunks.push(color_av1_data);
        } else {
            iloc_items.push(IlocItem {
                id: color_image_id,
                extents: [
                    IlocExtent {
                        offset: IlocOffset::Relative(0),
                        len: color_av1_data.len(),
                    },
                ].into(),
            });
            data_chunks.push(color_av1_data);
        };

        compatible_brands.push(FourCC(*b"mif1"));
        compatible_brands.push(FourCC(*b"miaf"));
        let mut boxes = AvifFile {
            ftyp: FtypBox {
                major_brand: FourCC(*b"avif"),
                minor_version: 0,
                compatible_brands,
            },
            meta: MetaBox {
                hdlr: HdlrBox {},
                iinf: IinfBox { items: image_items },
                pitm: PitmBox(color_image_id),
                iloc: IlocBox { items: iloc_items },
                iprp: IprpBox {
                    ipco,
                    // It's not enough to define these properties,
                    // they must be assigned to the image
                    ipma: IpmaBox {
                        entries: ipma_entries,
                    },
                },
                iref: irefs,
            },
            // Here's the actual data. If HEIF wasn't such a kitchen sink, this
            // would have been the only data this file needs.
            mdat: MdatBox {
                data_chunks: &data_chunks,
            },
        };

        boxes.write(into_output)
    }

    pub fn to_vec(&self, color_av1_data: &[u8], alpha_av1_data: Option<&[u8]>, width: u32, height: u32, depth_bits: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(color_av1_data.len() + alpha_av1_data.map_or(0, |a| a.len()) + 400);
        self.write(&mut out, color_av1_data, alpha_av1_data, width, height, depth_bits).unwrap(); // Vec can't fail
        out
    }
}

/// See [`serialize`] for description. This one makes a `Vec` instead of using `io::Write`.
pub fn serialize_to_vec(color_av1_data: &[u8], alpha_av1_data: Option<&[u8]>, width: u32, height: u32, depth_bits: u8) -> Vec<u8> {
    Aviffy::new().to_vec(color_av1_data, alpha_av1_data, width, height, depth_bits)
}

#[test]
fn test_roundtrip_parse_mp4() {
    let test_img = b"av12356abc";
    let avif = serialize_to_vec(test_img, None, 10, 20, 8);

    let ctx = mp4parse::read_avif(&mut avif.as_slice(), mp4parse::ParseStrictness::Normal).unwrap();

    assert_eq!(&test_img[..], ctx.primary_item_coded_data());
}

#[test]
fn test_roundtrip_parse_mp4_alpha() {
    let test_img = b"av12356abc";
    let test_a = b"alpha";
    let avif = serialize_to_vec(test_img, Some(test_a), 10, 20, 8);

    let ctx = mp4parse::read_avif(&mut avif.as_slice(), mp4parse::ParseStrictness::Normal).unwrap();

    assert_eq!(&test_img[..], ctx.primary_item_coded_data());
    assert_eq!(&test_a[..], ctx.alpha_item_coded_data());
}

#[test]
fn test_roundtrip_parse_avif() {
    let test_img = [1,2,3,4,5,6];
    let test_alpha = [77,88,99];
    let avif = serialize_to_vec(&test_img, Some(&test_alpha), 10, 20, 8);

    let ctx = avif_parse::read_avif(&mut avif.as_slice()).unwrap();

    assert_eq!(&test_img[..], ctx.primary_item.as_slice());
    assert_eq!(&test_alpha[..], ctx.alpha_item.as_deref().unwrap());
}

#[test]
fn premultiplied_flag() {
    let test_img = [1,2,3,4];
    let test_alpha = [55,66,77,88,99];
    let avif = Aviffy::new().premultiplied_alpha(true).to_vec(&test_img, Some(&test_alpha), 5, 5, 8);

    let ctx = avif_parse::read_avif(&mut avif.as_slice()).unwrap();

    assert!(ctx.premultiplied_alpha);
    assert_eq!(&test_img[..], ctx.primary_item.as_slice());
    assert_eq!(&test_alpha[..], ctx.alpha_item.as_deref().unwrap());
}
