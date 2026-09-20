//! Generate texture mip chains once on load, including embedded glTF images.
use bevy::{
    image::{ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::render_resource::{TextureDimension, TextureFormat, TextureUsages},
};

pub struct MipmapsPlugin;
impl Plugin for MipmapsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, mipmap_loaded_images);
    }
}

fn mipmap_loaded_images(
    mut events: MessageReader<AssetEvent<Image>>,
    mut images: ResMut<Assets<Image>>,
) {
    for event in events.read() {
        let (AssetEvent::Added { id } | AssetEvent::Modified { id }) = *event else {
            continue;
        };
        // Inspect without marking it modified; our own modification event must terminate.
        if images.get(id).is_some_and(eligible) {
            generate(&mut images.get_mut(id).unwrap());
        }
    }
}

fn eligible(image: &Image) -> bool {
    let desc = &image.texture_descriptor;
    desc.dimension == TextureDimension::D2
        && desc.size.depth_or_array_layers == 1
        && desc.size.width.max(desc.size.height) > 1
        && desc.mip_level_count == 1
        && !desc.usage.contains(TextureUsages::RENDER_ATTACHMENT)
        && matches!(
            desc.format,
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb
        )
        && image
            .data
            .as_ref()
            .is_some_and(|data| data.len() == (desc.size.width * desc.size.height * 4) as usize)
}

fn linear_to_srgb(value: f32) -> u8 {
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

fn generate(image: &mut Image) {
    let srgb = image.texture_descriptor.format == TextureFormat::Rgba8UnormSrgb;
    // Decode once into a lookup table. Color textures average in linear light;
    // normal/roughness/metalness textures remain linear data.
    let decode: [f32; 256] = std::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        if !srgb {
            v
        } else if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    let (mut width, mut height) = (image.width() as usize, image.height() as usize);
    let data = image.data.as_mut().unwrap();
    let mut offset = 0;
    let mut levels = 1;
    while width > 1 || height > 1 {
        let (next_w, next_h) = ((width / 2).max(1), (height / 2).max(1));
        let next_offset = data.len();
        for y in 0..next_h {
            for x in 0..next_w {
                let mut color = Vec3::ZERO;
                let mut alpha = 0.0;
                let mut count = 0.0;
                // Include the last row/column of odd-sized source images.
                for sy in y * height / next_h..(y + 1) * height / next_h {
                    for sx in x * width / next_w..(x + 1) * width / next_w {
                        let i = offset + (sy * width + sx) * 4;
                        let a = data[i + 3] as f32 / 255.0;
                        let rgb = Vec3::new(
                            decode[data[i] as usize],
                            decode[data[i + 1] as usize],
                            decode[data[i + 2] as usize],
                        );
                        // Transparent decals need alpha-weighted color to avoid dark fringes.
                        color += rgb * if srgb { a } else { 1.0 };
                        alpha += a;
                        count += 1.0;
                    }
                }
                color /= if srgb { alpha.max(f32::EPSILON) } else { count };
                for channel in color.to_array() {
                    data.push(if srgb {
                        linear_to_srgb(channel)
                    } else {
                        (channel * 255.0).round() as u8
                    });
                }
                data.push((alpha / count * 255.0).round() as u8);
            }
        }
        offset = next_offset;
        width = next_w;
        height = next_h;
        levels += 1;
    }
    image.texture_descriptor.mip_level_count = levels;
    let mut sampler = match &image.sampler {
        ImageSampler::Descriptor(sampler) => sampler.clone(),
        ImageSampler::Default => ImageSamplerDescriptor::default(),
    };
    sampler.min_filter = ImageFilterMode::Linear;
    sampler.mag_filter = ImageFilterMode::Linear;
    sampler.mipmap_filter = ImageFilterMode::Linear;
    sampler.anisotropy_clamp = 4;
    image.sampler = ImageSampler::Descriptor(sampler);
}

#[cfg(test)]
mod tests {
    use bevy::{asset::RenderAssetUsages, render::render_resource::Extent3d};

    use super::*;

    fn texture(width: u32, height: u32, pixels: Vec<u8>, srgb: bool) -> Image {
        Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            pixels,
            if srgb {
                TextureFormat::Rgba8UnormSrgb
            } else {
                TextureFormat::Rgba8Unorm
            },
            RenderAssetUsages::default(),
        )
    }

    #[test]
    fn color_mips_average_in_linear_light_and_preserve_wrap() {
        let mut image = texture(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255], true);
        let sampler = ImageSamplerDescriptor {
            address_mode_u: bevy::image::ImageAddressMode::Repeat,
            ..default()
        };
        image.sampler = ImageSampler::Descriptor(sampler);
        assert!(eligible(&image));
        generate(&mut image);
        assert_eq!(&image.data.as_ref().unwrap()[8..], &[188, 188, 188, 255]);
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        assert!(
            !eligible(&image),
            "must not regenerate on our own Modified event"
        );
        let ImageSampler::Descriptor(sampler) = image.sampler else {
            panic!()
        };
        assert_eq!(
            sampler.address_mode_u,
            bevy::image::ImageAddressMode::Repeat
        );
        assert_eq!(sampler.mipmap_filter, ImageFilterMode::Linear);
    }

    #[test]
    fn transparent_edges_do_not_darken_decal_colors() {
        let mut image = texture(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0], true);
        generate(&mut image);
        assert_eq!(&image.data.as_ref().unwrap()[8..], &[255, 0, 0, 128]);
    }

    #[test]
    fn odd_sized_data_maps_include_edge_texels_and_complete_chain() {
        let mut pixels = vec![0; 5 * 3 * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        pixels[(5 * 3 - 1) * 4] = 255;
        let mut image = texture(5, 3, pixels, false);
        generate(&mut image);
        let data = image.data.unwrap();
        assert_eq!(data.len(), (5 * 3 + 2 + 1) * 4);
        assert_eq!(image.texture_descriptor.mip_level_count, 3);
        assert!(
            data[data.len() - 4] > 0,
            "last texel contributes to the mip chain"
        );
    }
}
