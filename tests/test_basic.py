"""
测试 win-recorder 库功能 - 基本测试
"""
import win_recorder
import mss

# 测试获取显示器尺寸
print("测试 get_monitor_size...")
width, height = win_recorder.WinRecorder.get_monitor_size(1)
print(f"主显示器尺寸: {width} x {height}")

# 测试创建录屏器
print("\n测试创建录屏器...")
recorder = win_recorder.WinRecorder(
    output_path="test_output.mp4",
    fps=10,
    audio=False,
    monitor=1
)
print(f"录屏器创建成功:")
print(f"  - 分辨率: {recorder.width} x {recorder.height}")
print(f"  - 帧率: {recorder.fps}")

# 测试开始录制
print("\n测试开始录制...")
try:
    recorder.start()
    print("录制已开始")

    # 使用 mss 截取屏幕
    sct = mss.MSS()
    monitor = sct.monitors[1]

    # 写入 10 帧
    print("写入 10 帧...")
    for i in range(10):
        screenshot = sct.grab(monitor)
        frame_data = bytearray(screenshot.bgra)
        recorder.write_frame(frame_data)
        print(f"  已写入第 {i + 1} 帧")

    # 测试结束录制
    print("\n测试结束录制...")
    recorder.stop()
    print("录制已结束")

    # 检查文件
    import os
    if os.path.exists("test_output.mp4"):
        file_size = os.path.getsize("test_output.mp4")
        print(f"输出文件大小: {file_size / 1024:.2f} KB")
        print("\n测试成功!")
    else:
        print("错误: 输出文件不存在")

except Exception as e:
    print(f"录制失败: {e}")
    import traceback
    traceback.print_exc()