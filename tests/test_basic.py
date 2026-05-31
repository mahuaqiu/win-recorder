"""
测试 win-recorder 库功能 - 基本测试
"""
import win_recorder
import mss
import os

# 测试获取显示器尺寸
print("测试 get_monitor_size...")
orig_width, orig_height = win_recorder.WinRecorder.get_monitor_size(1)
print(f"主显示器尺寸: {orig_width} x {orig_height}")

# 测试创建录屏器
print("\n测试创建录屏器...")
output_path = "test_output.mp4"
recorder = win_recorder.WinRecorder(
    output_path=output_path,
    fps=10,
    audio=False,
    monitor=1
)
print(f"录屏器创建成功")

# 测试开始录制
print("\n测试开始录制...")
try:
    recorder.start()
    print("录制已开始")

    # 获取对齐后的分辨率（编码器实际使用的尺寸）
    aligned_width = recorder.width
    aligned_height = recorder.height
    print(f"对齐后分辨率: {aligned_width} x {aligned_height}")

    # 使用 mss 截取屏幕
    with mss.mss() as sct:
        monitor_config = sct.monitors[1]  # 主显示器

        # 写入 10 帧
        print("写入 10 帧...")
        for i in range(10):
            screenshot = sct.grab(monitor_config)
            # screenshot.raw 是 BGRA 格式的 bytearray，尺寸为原始尺寸
            raw_frame = bytearray(screenshot.raw)

            # 扩展帧到对齐后的尺寸（如果需要）
            if aligned_width != orig_width or aligned_height != orig_height:
                # 创建对齐尺寸的缓冲区
                aligned_frame = bytearray(aligned_width * aligned_height * 4)
                # 复制原始数据到对齐缓冲区
                for row in range(orig_height):
                    src_offset = row * orig_width * 4
                    dst_offset = row * aligned_width * 4
                    aligned_frame[dst_offset:dst_offset + orig_width * 4] = raw_frame[src_offset:src_offset + orig_width * 4]
                    # 填充剩余部分（黑边）
                    for x in range(orig_width, aligned_width):
                        aligned_frame[dst_offset + x * 4] = 0      # B
                        aligned_frame[dst_offset + x * 4 + 1] = 0  # G
                        aligned_frame[dst_offset + x * 4 + 2] = 0  # R
                        aligned_frame[dst_offset + x * 4 + 3] = 255  # A
                # 填充底部多余行（黑边）
                for row in range(orig_height, aligned_height):
                    dst_offset = row * aligned_width * 4
                    for x in range(aligned_width):
                        aligned_frame[dst_offset + x * 4] = 0
                        aligned_frame[dst_offset + x * 4 + 1] = 0
                        aligned_frame[dst_offset + x * 4 + 2] = 0
                        aligned_frame[dst_offset + x * 4 + 3] = 255

                frame_data = aligned_frame
            else:
                frame_data = raw_frame

            recorder.write_frame(frame_data)
            print(f"  已写入第 {i + 1} 帧")

    # 测试结束录制
    print("\n测试结束录制...")
    recorder.stop()
    print("录制已结束")

    # 检查文件
    if os.path.exists(output_path):
        file_size = os.path.getsize(output_path)
        print(f"输出文件大小: {file_size / 1024:.2f} KB")
        print("\n测试成功!")
        # 清理测试文件
        os.unlink(output_path)
    else:
        print("错误: 输出文件不存在")

except Exception as e:
    print(f"录制失败: {e}")
    import traceback
    traceback.print_exc()