use crate::error::GyResult;
use image::{self, ImageFormat};
use std::{
    io::{BufRead, BufReader, BufWriter, Seek},
    path::PathBuf,
};
use tract_onnx::prelude::*;

type TractSimplePlan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct ImageSize {
    pub width: usize,
    pub height: usize,
}

pub struct ModelConfig {
    pub model_path: PathBuf,
    pub image_size: ImageSize,
    pub layer_name: Option<String>,
}

pub struct DefaultImageEmbed {
    model: TractSimplePlan,
    config: ModelConfig,
}

impl DefaultImageEmbed {
    pub fn new(config: ModelConfig) -> Self {
        let model = Self::load_model(&config);
        Self {
            model: model,
            config: config,
        }
    }

    //加载模型
    fn load_model(m: &ModelConfig) -> TractSimplePlan {
        let mut model = tract_onnx::onnx()
            .model_for_path(m.model_path.clone())
            .expect("not found file")
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec!(1, 3, m.image_size.width, m.image_size.height),
                ),
            )
            .unwrap();
        if let Some(layer_name) = m.layer_name.clone() {
            model = model.with_output_names(vec![layer_name]).unwrap()
        }
        model.into_optimized().unwrap().into_runnable().unwrap()
    }

    pub fn embed<R: BufRead + Seek>(&self, r: R, image_ext: &str) -> GyResult<Vec<f32>> {
        let image_format =
            ImageFormat::from_extension(image_ext).ok_or("not surrport extension")?;
        let im = image::load(r, image_format)?.to_rgb8();
        let resized = image::imageops::resize(
            &im,
            self.config.image_size.width as u32,
            self.config.image_size.height as u32,
            ::image::imageops::FilterType::Triangle,
        );
        let image: Tensor = tract_ndarray::Array4::from_shape_fn(
            (
                1,
                3,
                self.config.image_size.width,
                self.config.image_size.height,
            ),
            |(_, c, y, x)| {
                let mean = [0.485, 0.456, 0.406][c];
                let std = [0.229, 0.224, 0.225][c];
                (resized[(x as _, y as _)][c] as f32 / 255.0 - mean) / std
            },
        )
        .into();
        let result = self.model.run(tvec!(image))?;
        let best: Vec<f32> = result[0].to_array_view::<f32>()?.iter().cloned().collect();
        Ok(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::BufReader;

    #[test]
    fn test_embed() {
        let dir_path = std::env::current_dir().unwrap();

        println!("{:?}", dir_path);
        let model_path = dir_path.join("../model").join("mobilenetv2-7.onnx");
        let config = ModelConfig {
            model_path: model_path,
            image_size: ImageSize {
                width: 224,
                height: 224,
            },
            layer_name: Some("Reshape_103".to_string()),
        };
        let image_path = dir_path.join("images").join("cat.jpeg");
        let model = DefaultImageEmbed::new(config);
        let f = File::open(image_path).unwrap(); // Read<[u8]>
        let f = BufReader::new(f);
        let res = model.embed(f, "jpeg").unwrap();
    }
}
