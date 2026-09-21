---
title: Root View
description: 使用 Root 视图为窗口启用主题、通知、对话框及其他 GPUI Component 功能。
example: false
---

# Root View

[Root] 是 GPUI Kit 窗口的根视图。它持有窗口的对话框、抽屉和通知并把它们渲染在所包裹的视图之上，也承载 tooltip 与菜单。`gpui_kit::open_window` 会替你创建它；只有窗口需要定制 `Root` 时才会直接接触它。

下面这份完整的 **Tested consumer recipe** 在隔离的 `gpui-kit` 消费者工作区中编译。它先初始化 GPUI Kit，再打开一个以 `Root` 包裹应用视图为根的窗口。

<!-- recipe:bootstrap:start -->
```rust
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowOptions, div,
};

pub fn run() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            // The window's root view is a `Root` wrapping the view, which
            // renders dialogs, sheets and notifications above it.
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| BootstrapView)
            })
            .expect("failed to open window");
        });
}

struct BootstrapView;

impl Render for BootstrapView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("My application")
    }
}
```
<!-- recipe:bootstrap:end -->

## 定制 Root

`gpui_kit::open_window` 就是 `cx.open_window` 加上 `Root` 包裹。窗口需要配置 `Root` 时自己构造它——例如 layer-shell 全屏窗口不该画 GPUI Component 的客户端窗口边框，用 `bordered(false)`：

```rs
cx.open_window(WindowOptions::default(), |window, cx| {
    let view = cx.new(|_| MyApp);
    cx.new(|cx| Root::new(view, window, cx).bordered(false))
})
```

不论怎么构造，`Root` 都必须是窗口的根视图：`window.open_dialog`、`open_sheet`、`push_notification` 把状态存在它上面，缺少它时会 panic 并指向本页。

`open_window` 同时返回窗口和视图，所以必须在窗口内构造的视图（比如它持有 `InputState`）也能留住句柄：

```rust
let (window, editor) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
    cx.new(|cx| Editor::new(window, cx))
})?;
```

## 关闭窗口与退出应用

应用自行定义退出和关闭窗口的 action 及快捷键，`gpui_kit::init` 不会安装这些绑定。应用应在关闭窗口或退出之前处理未保存的内容及确认流程。

## 浮层

`Root` 统一挂载对话框、抽屉和通知层，始终渲染在应用内容之上。应用只需调用 `window.open_dialog`、`window.open_sheet` 或 `window.push_notification`，不需要手动挂载或配置开关。子视图是否缓存不影响浮层渲染。

### 迁移到 0.7.0

`Root::render_dialog_layer`、`Root::render_sheet_layer` 和 `Root::render_notification_layer` 已删除。删除视图中对应的调用及 `.children(...)` 即可。此前自定义的层位置统一改为窗口级 Root 的浮层位置。

[Root]: https://docs.rs/gpui-component/latest/gpui_component/root/struct.Root.html
