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

    # 创建录屏器
    recorder = win_recorder.WinRecorder(
        output_path=output_path,
        fps=fps,
        audio=False,
        monitor=1
    )

    print(f"录屏器创建成功:")
    print(f"  - 输出文件: {output_path}")
    print(f"  - 分辨率: {recorder.width} x {recorder.height}")
    print(f"  - 帧率: {fps}")

    # 使用 mss 截取屏幕
    sct = mss.MSS()
    monitor = sct.monitors[1]  # 1 是主显示器

    # 开始录制
    print("开始录制...")
    recorder.start()
    print("录制已开始")

    frame_count = int(fps * duration)
    for i in range(frame_count):
        # 截取屏幕
        screenshot = sct.grab(monitor)

        # 获取 BGRA 数据，转换为 bytearray
        frame_data = bytearray(screenshot.bgra)

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