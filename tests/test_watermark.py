"""测试时间水印功能"""
import win_recorder
import mss
import os
import time

print("=== 测试时间水印功能 ===")

# 测试获取显示器尺寸
orig_width, orig_height = win_recorder.WinRecorder.get_monitor_size(1)
print(f"主显示器尺寸: {orig_width} x {orig_height}")

# 测试创建带水印的录屏器
output_path = "test_watermark.mp4"
recorder = win_recorder.WinRecorder(
    output_path=output_path,
    fps=10,
    audio=False,
    monitor=1,
    watermark=True  # 开启水印
)
print("录屏器创建成功 (watermark=True)")

# 开始录制
recorder.start()
print("录制已开始")

# 获取对齐后的分辨率
aligned_width = recorder.width
aligned_height = recorder.height
print(f"对齐后分辨率: {aligned_width} x {aligned_height}")

# 使用 mss 截取屏幕，录制 10 秒
with mss.MSS() as sct:
    monitor_config = sct.monitors[1]

    start_time = time.time()
    last_log_time = start_time
    frame_count = 0

    while time.time() - start_time < 10:  # 录制 10 秒
        screenshot = sct.grab(monitor_config)
        raw_frame = bytearray(screenshot.raw)

        # 扩展帧到对齐尺寸
        if aligned_width != orig_width or aligned_height != orig_height:
            aligned_frame = bytearray(aligned_width * aligned_height * 4)
            for row in range(orig_height):
                src_offset = row * orig_width * 4
                dst_offset = row * aligned_width * 4
                aligned_frame[dst_offset:dst_offset + orig_width * 4] = raw_frame[src_offset:src_offset + orig_width * 4]
            frame_data = aligned_frame
        else:
            frame_data = raw_frame

        recorder.write_frame(frame_data)
        frame_count += 1

        # 每秒打印一次进度
        now = time.time()
        if now - last_log_time >= 1.0:
            elapsed = now - start_time
            print(f"  已写入 {frame_count} 帧 ({elapsed:.1f}s)")
            last_log_time = now

# 结束录制
recorder.stop()
print(f"录制已结束，共 {frame_count} 帧")

# 检查文件
if os.path.exists(output_path):
    file_size = os.path.getsize(output_path)
    print(f"输出文件: {output_path}")
    print(f"文件大小: {file_size / 1024 / 1024:.2f} MB")
    print("\n请检查视频左下角是否显示时间水印 HH:MM:SS.mmm")
    # 不删除，方便用户检查
else:
    print("错误: 输出文件不存在")
