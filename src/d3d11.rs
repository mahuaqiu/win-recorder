use crate::error::RecorderError;
use std::ptr;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;

/// D3D11 纹理管理器
///
/// 实现双纹理架构：
/// - Staging 纹理：CPU 可写，用于上传帧数据
/// - GPU 纹理：DEFAULT + SHARED，用于 MF SinkWriter 编码
pub struct D3D11TextureManager {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging_texture: ID3D11Texture2D,
    gpu_texture: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl D3D11TextureManager {
    /// 创建 D3D11 纹理管理器
    ///
    /// # 参数
    /// - width: 纹理宽度
    /// - height: 纹理高度
    ///
    /// # 返回
    /// 成功返回 D3D11TextureManager 实例
    pub fn new(width: u32, height: u32) -> Result<Self, RecorderError> {
        unsafe {
            // 创建 D3D11 设备
            let mut device = None;
            let mut context = None;
            let mut feature_level = D3D_FEATURE_LEVEL_9_1;

            let feature_levels = [
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_10_0,
                D3D_FEATURE_LEVEL_9_3,
                D3D_FEATURE_LEVEL_9_2,
                D3D_FEATURE_LEVEL_9_1,
            ];

            D3D11CreateDevice(
                None, // 默认适配器
                D3D_DRIVER_TYPE_HARDWARE,
                None, // 软件模块
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT, // 支持视频
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
            .map_err(|e| RecorderError::D3D11Error(format!("创建 D3D11 设备失败: {}", e)))?;

            let device = device.ok_or_else(|| {
                RecorderError::D3D11Error("创建 D3D11 设备返回空指针".to_string())
            })?;
            let context = context.ok_or_else(|| {
                RecorderError::D3D11Error("创建 D3D11 上下文返回空指针".to_string())
            })?;

            // 创建 Staging 纹理（CPU 可读写）
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0, // Staging 纹理不需要绑定到渲染管线
                // 设置读写权限，以便后续从纹理读取数据
                CPUAccessFlags: (D3D11_CPU_ACCESS_WRITE.0 | D3D11_CPU_ACCESS_READ.0) as u32,
                MiscFlags: 0,
            };

            let mut staging_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture as *mut _))
                .map_err(|e| {
                    RecorderError::D3D11TextureError(format!("创建 Staging 纹理失败: {}", e))
                })?;

            let staging_texture = staging_texture.ok_or_else(|| {
                RecorderError::D3D11TextureError("创建 Staging 纹理返回空指针".to_string())
            })?;

            // 创建 GPU 纹理（DEFAULT + SHARED）
            let gpu_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                // 使用 SHADER_RESOURCE 作为通用绑定标志，兼容性更好
                // 如果需要视频编码，可以尝试 VIDEO_ENCODER，但可能需要特定硬件支持
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
            };

            let mut gpu_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&gpu_desc, None, Some(&mut gpu_texture as *mut _))
                .map_err(|e| {
                    RecorderError::D3D11TextureError(format!("创建 GPU 纹理失败: {}", e))
                })?;

            let gpu_texture = gpu_texture.ok_or_else(|| {
                RecorderError::D3D11TextureError("创建 GPU 纹理返回空指针".to_string())
            })?;

            Ok(Self {
                device,
                context,
                staging_texture,
                gpu_texture,
                width,
                height,
            })
        }
    }

    /// 仅上传 BGRA 帧数据到 Staging 纹理（不拷贝到 GPU）
    ///
    /// # 参数
    /// - frame_data: BGRA 格式的帧数据（每像素 4 字节）
    ///
    /// # 说明
    /// 仅上传数据到 staging 纹理（不拷贝到 GPU）
    /// 注意：使用 D3D11_MAP_READ_WRITE 以便后续水印绘制可以单独 Map 此纹理
    pub fn upload_bgra_to_staging(&self, frame_data: &[u8]) -> Result<(), RecorderError> {
        unsafe {
            let expected_size = (self.width * self.height * 4) as usize;
            if frame_data.len() != expected_size {
                return Err(RecorderError::FrameSizeMismatch {
                    expected: expected_size,
                    actual: frame_data.len(),
                });
            }

            // 映射 Staging 纹理（READ_WRITE 以便后续水印绘制可在同一 Map 周期内完成）
            let mut mapped_resource = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ_WRITE,
                    0,
                    Some(&mut mapped_resource),
                )
                .map_err(|e| {
                    RecorderError::D3D11TextureError(format!("映射 Staging 纹理失败: {}", e))
                })?;

            // 逐行拷贝数据（考虑 row pitch）
            let src_pitch = self.width * 4;
            let dst_pitch = mapped_resource.RowPitch as usize;

            for row in 0..self.height as usize {
                let src_offset = row * src_pitch as usize;
                let dst_offset = row * dst_pitch;

                ptr::copy_nonoverlapping(
                    frame_data.as_ptr().add(src_offset),
                    mapped_resource.pData.add(dst_offset) as *mut u8,
                    src_pitch as usize,
                );
            }

            // 解除映射
            self.context.Unmap(&self.staging_texture, 0);

            Ok(())
        }
    }

    /// 将 Staging 纹理拷贝到 GPU 纹理
    pub fn copy_staging_to_gpu(&self) {
        unsafe {
            self.context
                .CopyResource(&self.gpu_texture, &self.staging_texture);
        }
    }

    /// 创建 Media Foundation Sample（使用内存缓冲区）
    ///
    /// # 返回
    /// 返回包含帧数据的 IMFSample
    ///
    /// # 说明
    /// 用于传递给 MF SinkWriter 进行编码
    /// 注意：此方法直接从已上传的帧数据创建缓冲区
    /// 由于数据已经在 Staging 纹理中，我们需要重新映射以读取
    pub fn create_mf_sample(&self) -> Result<IMFSample, RecorderError> {
        unsafe {
            // 创建 Sample
            let sample = MFCreateSample()
                .map_err(|e| RecorderError::MFError(format!("创建 IMFSample 失败: {}", e)))?;

            // 计算缓冲区大小
            let buffer_size = (self.width * self.height * 4) as u32;

            // 创建内存缓冲区
            let buffer = MFCreateMemoryBuffer(buffer_size)
                .map_err(|e| RecorderError::MFError(format!("创建 Memory Buffer 失败: {}", e)))?;

            // 锁定缓冲区以写入数据
            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut max_length = 0u32;
            let mut current_length = 0u32;
            buffer
                .Lock(&mut data_ptr, Some(&mut max_length), Some(&mut current_length))
                .map_err(|e| RecorderError::MFError(format!("锁定缓冲区失败: {}", e)))?;

            // 映射 GPU 纹理以读取数据（使用 MAP_READ）
            // 注意：GPU 纹理是 DEFAULT usage，不能直接映射
            // 我们需要使用 Staging 纹理来读取数据
            let mut mapped_resource = D3D11_MAPPED_SUBRESOURCE::default();

            // 先将 GPU 纹理拷贝回 Staging 纹理
            self.context.CopyResource(&self.staging_texture, &self.gpu_texture);

            self.context
                .Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped_resource),
                )
                .map_err(|e| RecorderError::MFError(format!("映射 Staging 纹理失败: {}", e)))?;

            // 从 Staging 纹理拷贝数据到缓冲区
            let src_pitch = (self.width * 4) as usize;
            let dst_pitch = (self.width * 4) as usize;
            for row in 0..self.height as usize {
                let src_offset = row * src_pitch;
                let dst_offset = row * dst_pitch;
                ptr::copy_nonoverlapping(
                    mapped_resource.pData.add(src_offset) as *const u8,
                    data_ptr.add(dst_offset),
                    src_pitch,
                );
            }

            // 解除映射
            self.context.Unmap(&self.staging_texture, 0);

            // 设置当前长度
            buffer
                .SetCurrentLength(buffer_size)
                .map_err(|e| RecorderError::MFError(format!("设置缓冲区长度失败: {}", e)))?;

            // 解锁缓冲区
            buffer
                .Unlock()
                .map_err(|e| RecorderError::MFError(format!("解锁缓冲区失败: {}", e)))?;

            // 添加 Buffer 到 Sample
            sample
                .AddBuffer(&buffer)
                .map_err(|e| RecorderError::MFError(format!("添加 Buffer 到 Sample 失败: {}", e)))?;

            Ok(sample)
        }
    }

    /// 检测显示器尺寸
    ///
    /// # 参数
    /// - `monitor`: 显示器选择（1=主屏幕 left=0，2=副屏幕）
    ///
    /// # 返回
    /// 返回显示器尺寸 (width, height)
    ///
    /// # 错误
    /// - `MonitorNotFound`: 指定显示器不存在
    /// - `InvalidParam`: monitor 参数无效（必须为 1 或 2）
    pub fn detect_monitor(monitor: u32) -> Result<(u32, u32), RecorderError> {
        if monitor != 1 && monitor != 2 {
            return Err(RecorderError::InvalidParam("monitor must be 1 or 2".into()));
        }

        unsafe {
            // 创建 DXGI 工厂
            let factory: IDXGIFactory1 = CreateDXGIFactory1()
                .map_err(|e| RecorderError::D3D11Error(format!("创建 DXGI 工厂失败: {}", e)))?;

            // 收集所有显示器
            let mut outputs: Vec<IDXGIOutput> = Vec::new();
            let mut adapter_index = 0u32;
            loop {
                let adapter = match factory.EnumAdapters1(adapter_index) {
                    Ok(a) => a,
                    Err(_) => break,
                };

                let mut output_index = 0u32;
                loop {
                    let output = match adapter.EnumOutputs(output_index) {
                        Ok(o) => o,
                        Err(_) => break,
                    };
                    outputs.push(output);
                    output_index += 1;
                }
                adapter_index += 1;
            }

            // 按 left 坐标排序
            let mut desc_list: Vec<DXGI_OUTPUT_DESC> = outputs
                .iter()
                .map(|o| o.GetDesc())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| RecorderError::D3D11Error(format!("获取显示器描述失败: {}", e)))?;
            desc_list.sort_by_key(|d| d.DesktopCoordinates.left);

            match monitor {
                1 => {
                    // 主屏幕：left=0 的显示器
                    let primary = desc_list
                        .iter()
                        .find(|d| d.DesktopCoordinates.left == 0)
                        .ok_or(RecorderError::MonitorNotFound { monitor })?;
                    let rect = primary.DesktopCoordinates;
                    Ok(((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32))
                }
                2 => {
                    // 副屏幕：另一个显示器
                    let secondary = desc_list
                        .iter()
                        .find(|d| d.DesktopCoordinates.left != 0)
                        .ok_or(RecorderError::MonitorNotFound { monitor })?;
                    let rect = secondary.DesktopCoordinates;
                    Ok(((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32))
                }
                _ => Err(RecorderError::InvalidParam("monitor must be 1 or 2".into())),
            }
        }
    }

    /// 获取纹理宽度
    #[allow(dead_code)]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 获取纹理高度
    #[allow(dead_code)]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 获取 D3D11 设备上下文（用于水印绘制）
    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// 获取 Staging 纹理（用于水印绘制）
    pub fn staging_texture(&self) -> &ID3D11Texture2D {
        &self.staging_texture
    }

    /// 获取 D3D11 设备
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// 获取 GPU 纹理
    #[allow(dead_code)]
    pub fn gpu_texture(&self) -> &ID3D11Texture2D {
        &self.gpu_texture
    }
}