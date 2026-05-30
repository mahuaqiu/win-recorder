"""
测试 win-recorder 库功能 - 最小测试
"""
import win_recorder

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
print(f"  - 是否录制: {recorder.is_recording}")

# 测试开始录制
print("\n测试开始录制...")
try:
    recorder.start()
    print("录制已开始")
    print(f"  - 是否录制: {recorder.is_recording}")

    # 测试结束录制
    print("\n测试结束录制...")
    recorder.stop()
    print("录制已结束")
    print(f"  - 是否录制: {recorder.is_recording}")

except Exception as e:
    print(f"录制失败: {e}")
    import traceback
    traceback.print_exc()