"""
测试 win-recorder 库功能 - 简化版本
"""
import win_recorder
import mss
import time
import os

def test_full_recording():
    """测试完整录制流程"""
    print("\n测试完整录制流程...")

    output_path = "test_output.mp4"
    fps = 10  # 降低帧率以减少内存占用
    duration = 2  # 录制 2 秒

    # 获取原始显示器尺寸
    orig_width, orig_height = win_recorder.WinRecorder.get_monitor_size(1)
    print(f"原始分辨率: {orig_width} x {orig_height}")

    # 创建录屏器
    recorder = win_recorder.WinRecorder(
        output_path=output_path,
        fps=fps,
        audio=False,
        monitor=1
    )

    print(f"录屏器创建成功:")
    print(f"  - 输出文件: {output_path}")
    print(f"  - 原始分辨率: {recorder.width} x {recorder.height}")
    print(f"  - 帧率: {fps}")

    # 使用 mss 截取屏幕
    sct = mss.MSS()
    monitor_config = sct.monitors[1]  # 1 是主显示器

    # 开始录制
    print("开始录制...")
    recorder.start()
    print("录制已开始")

    # 获取对齐后的分辨率（编码器实际使用的尺寸）
    aligned_width = recorder.width
    aligned_height = recorder.height
    print(f"对齐后分辨率: {aligned_width} x {aligned_height}")

    frame_count = int(fps * duration)
    for i in range(frame_count):
        # 截取屏幕
        screenshot = sct.grab(monitor_config)

        # 获取 BGRA 数据
        raw_frame = bytearray(screenshot.bgra)

        # 如果需要，扩展帧数据到对齐后的尺寸
        if aligned_width != orig_width or aligned_height != orig_height:
            # 创建对齐尺寸的缓冲区（填充黑边）
            aligned_frame = bytearray(aligned_width * aligned_height * 4)

            # 复制原始数据（逐行）
            for row in range(orig_height):
                src_offset = row * orig_width * 4
                dst_offset = row * aligned_width * 4
                aligned_frame[dst_offset:dst_offset + orig_width * 4] = raw_frame[src_offset:src_offset + orig_width * 4]

                # 填充右侧黑边（如果有）
                for x in range(orig_width, aligned_width):
                    aligned_frame[dst_offset + x * 4] = 0      # B
                    aligned_frame[dst_offset + x * 4 + 1] = 0  # G
                    aligned_frame[dst_offset + x * 4 + 2] = 0  # R
                    aligned_frame[dst_offset + x * 4 + 3] = 255  # A

            # 填充底部黑边
            for row in range(orig_height, aligned_height):
                dst_offset = row * aligned_width * 4
                for x in range(aligned_width):
                    aligned_frame[dst_offset + x * 4] = 0      # B
                    aligned_frame[dst_offset + x * 4 + 1] = 0  # G
                    aligned_frame[dst_offset + x * 4 + 2] = 0  # R
                    aligned_frame[dst_offset + x * 4 + 3] = 255  # A

            frame_data = aligned_frame
        else:
            frame_data = raw_frame

        # 写入帧
        recorder.write_frame(frame_data)

        # 显示进度
        if (i + 1) % fps == 0:
            print(f"  已录制 {i + 1} 帧 ({(i + 1) // fps} 秒)")

    # 结束录制
    print("结束录制...")
    recorder.stop()

    print(f"录制完成")

    # 检查文件是否存在
    if os.path.exists(output_path):
        file_size = os.path.getsize(output_path)
        print(f"输出文件大小: {file_size / 1024:.2f} KB")
        # 清理测试文件
        os.unlink(output_path)
        print("测试文件已清理")
    else:
        print("错误: 输出文件不存在")

if __name__ == "__main__":
    try:
        # 测试获取显示器尺寸
        print("测试 get_monitor_size...")
        width, height = win_recorder.WinRecorder.get_monitor_size(1)
        print(f"主显示器尺寸: {width} x {height}")

        # 测试完整录制流程
        test_full_recording()

        print("\n所有测试完成!")
    except Exception as e:
        print(f"\n测试失败: {e}")
        import traceback
        traceback.print_exc()