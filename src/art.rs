use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ArtMapping {
    empty: HashSet<char>,
    filled: Option<HashSet<char>>,
}

impl ArtMapping {
    pub fn from_cli(empty: Option<&str>, filled: Option<&str>) -> Self {
        let empty_set = empty.unwrap_or(" ").chars().collect();
        let filled_set = filled.map(|chars| chars.chars().collect());
        Self {
            empty: empty_set,
            filled: filled_set,
        }
    }

    fn is_filled(&self, ch: char) -> bool {
        if self.empty.contains(&ch) {
            return false;
        }
        if let Some(filled) = &self.filled {
            return filled.contains(&ch);
        }
        !ch.is_whitespace()
    }
}

impl Default for ArtMapping {
    fn default() -> Self {
        Self::from_cli(None, None)
    }
}

impl Bitmap {
    pub fn new(width: usize, height: usize, pixels: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("bitmap dimensions must be non-zero");
        }
        if pixels.len() != width * height {
            bail!(
                "bitmap has {} pixels but dimensions require {}",
                pixels.len(),
                width * height
            );
        }
        if pixels.iter().any(|pixel| *pixel > 1) {
            bail!("bitmap pixels must be 0 or 1");
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn blank(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    pub fn from_ascii(
        input: &str,
        width: usize,
        height: usize,
        mapping: &ArtMapping,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("width and height must be non-zero");
        }

        let raw_lines: Vec<Vec<u8>> = input
            .lines()
            .map(|line| {
                line.chars()
                    .map(|ch| u8::from(mapping.is_filled(ch)))
                    .collect()
            })
            .collect();

        if raw_lines.is_empty() {
            return Ok(Self::blank(width, height));
        }

        let raw_height = raw_lines.len();
        let raw_width = raw_lines.iter().map(Vec::len).max().unwrap_or(0);
        if raw_width == 0 {
            return Ok(Self::blank(width, height));
        }

        let mut grid = vec![0; raw_width * raw_height];
        for (y, line) in raw_lines.iter().enumerate() {
            for (x, pixel) in line.iter().enumerate() {
                grid[y * raw_width + x] = *pixel;
            }
        }

        let Some((min_x, max_x, min_y, max_y)) = bounding_box(&grid, raw_width, raw_height) else {
            return Ok(Self::blank(width, height));
        };

        let trimmed_width = max_x - min_x + 1;
        let trimmed_height = max_y - min_y + 1;
        let mut trimmed = vec![0; trimmed_width * trimmed_height];
        for y in 0..trimmed_height {
            for x in 0..trimmed_width {
                trimmed[y * trimmed_width + x] = grid[(min_y + y) * raw_width + min_x + x];
            }
        }

        Ok(resize_preserving_aspect(
            &trimmed,
            trimmed_width,
            trimmed_height,
            width,
            height,
        ))
    }

    pub fn from_digit_window(
        digits: &[u8],
        width: usize,
        height: usize,
        threshold: u8,
    ) -> Result<Self> {
        if digits.len() != width * height {
            bail!("digit window length does not match bitmap dimensions");
        }
        let pixels = digits
            .iter()
            .map(|digit| u8::from(*digit >= threshold))
            .collect();
        Self::new(width, height, pixels)
    }

    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }

    pub fn to_bit_string(&self) -> String {
        self.pixels
            .iter()
            .map(|pixel| if *pixel == 1 { '1' } else { '0' })
            .collect()
    }

    pub fn from_bit_string(width: usize, height: usize, bits: &str) -> Result<Self> {
        let pixels: Vec<u8> = bits
            .chars()
            .map(|ch| match ch {
                '0' => Ok(0),
                '1' => Ok(1),
                _ => Err(anyhow!("bitmap contains non-binary character {ch:?}")),
            })
            .collect::<Result<_>>()?;
        Self::new(width, height, pixels)
    }

    pub fn similarity(&self, other: &Self) -> Result<f64> {
        if self.width != other.width || self.height != other.height {
            bail!("cannot compare bitmaps with different dimensions");
        }
        let matching = self
            .pixels
            .iter()
            .zip(other.pixels.iter())
            .filter(|(left, right)| left == right)
            .count();
        Ok(matching as f64 / self.pixels.len() as f64)
    }

    pub fn sha256(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.width.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(&self.pixels);
        format!("{:x}", hasher.finalize())
    }

    pub fn render_ascii(&self, filled: char, empty: char) -> String {
        let mut out = String::new();
        for y in 0..self.height {
            for x in 0..self.width {
                out.push(if self.get(x, y) == 1 { filled } else { empty });
            }
            if y + 1 != self.height {
                out.push('\n');
            }
        }
        out
    }
}

fn bounding_box(grid: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let mut min_x = width;
    let mut max_x = 0;
    let mut min_y = height;
    let mut max_y = 0;
    let mut found = false;

    for y in 0..height {
        for x in 0..width {
            if grid[y * width + x] == 1 {
                found = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }

    found.then_some((min_x, max_x, min_y, max_y))
}

fn resize_preserving_aspect(
    source: &[u8],
    source_width: usize,
    source_height: usize,
    out_width: usize,
    out_height: usize,
) -> Bitmap {
    let x_scale = out_width as f64 / source_width as f64;
    let y_scale = out_height as f64 / source_height as f64;
    let scale = x_scale.min(y_scale);
    let scaled_width = ((source_width as f64 * scale).round() as usize).clamp(1, out_width);
    let scaled_height = ((source_height as f64 * scale).round() as usize).clamp(1, out_height);
    let x_offset = (out_width - scaled_width) / 2;
    let y_offset = (out_height - scaled_height) / 2;
    let mut out = Bitmap::blank(out_width, out_height);

    for y in 0..scaled_height {
        for x in 0..scaled_width {
            let src_x = (x * source_width / scaled_width).min(source_width - 1);
            let src_y = (y * source_height / scaled_height).min(source_height - 1);
            out.pixels[(y_offset + y) * out_width + x_offset + x] =
                source[src_y * source_width + src_x];
        }
    }

    out
}

pub fn template_names() -> &'static [&'static str] {
    &["arch", "pi"]
}

pub fn load_template(name: &str, width: usize, height: usize) -> Result<Bitmap> {
    let normalized_name = name.to_ascii_lowercase();
    match normalized_name.as_str() {
        "arch" => Bitmap::from_ascii(
            ARCH_TEMPLATE,
            width,
            height,
            &ArtMapping::from_cli(Some(" ."), Some("#")),
        ),
        "pi" => Bitmap::from_ascii(
            PI_TEMPLATE,
            width,
            height,
            &ArtMapping::from_cli(Some(" ."), Some("#")),
        ),
        other => bail!(
            "unknown template {other:?}; available templates: {}",
            template_names().join(", ")
        ),
    }
}

const PI_TEMPLATE: &str = r#"
      ################################  
    ##################################  
   ###################################  
   ####    #####         ####           
  ###      #####        #####           
  #        #####        #####           
  #        #####        ####            
           ####         ####            
           ####         ####            
           ####         ####            
          #####        #####            
          #####        #####            
          ####         #####            
          ####         #####            
         #####         #####            
        ######         #####            
        #####          #####        ##  
       #######         ######       ##  
      #######           ##############  
     #######            #############   
     #######             ###########    
     ######               #########     
"#;

const ARCH_TEMPLATE: &str = r#"
                  ##
                 ####
                ######
               ########
              ##########
             ############
            ##############
           ################
          ##################
         ####################
        ######################
       #########      #########
      ##########      ##########
     ###########      ###########
    ##########          ##########
   #######                  #######
  ####                          ####
 ###                              ###
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ascii_with_default_mapping() {
        let bitmap = Bitmap::from_ascii(" .#\n  #", 3, 2, &ArtMapping::default()).unwrap();
        assert_eq!(bitmap.width, 3);
        assert_eq!(bitmap.height, 2);
        assert!(bitmap.pixels.contains(&1));
    }

    #[test]
    fn explicit_mapping_can_make_dots_empty() {
        let mapping = ArtMapping::from_cli(Some(" ."), Some("#"));
        let bitmap = Bitmap::from_ascii(".#.", 3, 1, &mapping).unwrap();
        assert_eq!(bitmap.pixels, vec![0, 1, 0]);
    }

    #[test]
    fn digit_window_uses_threshold() {
        let bitmap = Bitmap::from_digit_window(&[0, 4, 5, 9], 2, 2, 5).unwrap();
        assert_eq!(bitmap.pixels, vec![0, 0, 1, 1]);
    }

    #[test]
    fn similarity_detects_exact_match() {
        let a = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        let b = Bitmap::new(2, 2, vec![0, 1, 1, 0]).unwrap();
        assert_eq!(a.similarity(&b).unwrap(), 1.0);
    }

    #[test]
    fn loads_templates() {
        for name in template_names() {
            let bitmap = load_template(name, 16, 16).unwrap();
            assert_eq!(bitmap.pixels.len(), 256);
            assert!(bitmap.pixels.contains(&1));
        }
    }

    #[test]
    fn loads_template_size_modes() {
        for name in template_names() {
            for size in [8, 12, 16] {
                let bitmap = load_template(name, size, size).unwrap();
                assert_eq!(bitmap.width, size);
                assert_eq!(bitmap.height, size);
                assert_eq!(bitmap.pixels.len(), size * size);
                assert!(bitmap.pixels.contains(&1));
                assert!(bitmap.pixels.contains(&0));
            }
        }
    }

    #[test]
    fn unknown_builtin_template_is_rejected() {
        assert_eq!(template_names(), &["arch", "pi"]);
        assert!(load_template("unknown", 16, 16).is_err());
    }
}
