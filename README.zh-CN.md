# Crwm

Crwm 是 “Classical river window manager” 的简写。这是一个模仿 [river-classic](https://codeberg.org/river/river-classic) 可以自由配置布局的风格，由 Rust 编译，为 [river](https://codeberg.org/river/river) wayland 合成器打造的窗口管理器。

该项目参考了 [tinyrwm](https://codeberg.org/river/tinyrwm) 的 Rust 和 C 版本代码，使用了 deepseek-flash 搜索资料和编写代码。

# Feature

# Building

```text
crwm
 |- src -|
         |- main.rs 解析命令行参数，处理返回错误
         |- lib.rs 处理 RiverWindowManagerV1 协议和接收所有模块配置
         |
         |- wm -|
         |      |- window.rs 处理 RiverWindowV1 协议
         |      |- output.rs 处理 RiverOutputV1 协议
         |      |- seat.rs 处理 RiverSeatV1 协议
         |
         |- config -|
                    |- parse.rs 解析 toml 配置和基础配置
                    |- binding.rs 处理键位绑定
                    |- layout.rs 处理用户自定义的布局、工作区和规则
                    |- render.rs 处理渲染参数
```

# Usage
