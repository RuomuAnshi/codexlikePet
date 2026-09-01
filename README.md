# SakiPet

一个基于 Tauri 2 + Rust + Vite/TypeScript 的透明桌面宠物。使用类似codex格式的桌宠。

## 开发

```bash
npm install
npm run tauri dev
```
## 管理入口

启动后可以通过以下入口打开宠物管理：

- 桌宠图标的托盘菜单 → 管理宠物；
- 菜单栏的 SakiPet 图标 → 管理宠物；
- macOS 顶部菜单栏的 SakiPet → 管理宠物。

宠物管理和 AI 设置是两个独立窗口。宠物管理页只负责宠物资源、显示实例和每只宠物的独立设置；点击其中的“AI 设置”按钮，或从 SakiPet 菜单选择“AI 设置”，可以打开全局 AI 陪伴设置。

双击桌宠会打开（或聚焦）这只宠物独立的聊天窗口；单击、拖拽、散步和长时间
idle 仍然使用 `character.json` 中的静态台词。

管理页支持：

- 同时显示不同种类的宠物，每种宠物最多一个实例；
- 单独显示、隐藏和移除实例；
- 停用/启用宠物资源；
- 为每种宠物分别调整大小、透明度、行走速度、散步、安静模式、位置锁定、点击穿透、全屏显示和暂停状态；
- 导入、校验和删除宠物资源。

“社交设置”与“宠物日志”是独立窗口：可以调整自动社交的 3–10 分钟间隔、靠近触发、
手动触发、最多参与宠物数和共享道具；日志会显示宠物之间的场景、公开台词、关系等级
和里程碑。右键桌宠也可以直接选择“找朋友”。

配置保存于 Tauri 的应用配置目录，宠物导入文件保存于应用数据目录下的 `pets/`。

## 导入宠物包

管理页的“导入宠物包”接受 `.zip` 文件。压缩包内需要包含一个 `pet.json`
和对应的 V2 spritesheet，例如：

```text
my-pet.zip
└── my-pet/
    ├── pet.json
    ├── character.json  # 可选
    ├── spritesheet.webp
    ├── animations.json  # 可选，SakiPet 扩展帧动画索引
    └── animations/      # 可选，每个动作是完整的 192×208 帧条
```

可以额外放入 `character.json` 配置角色卡和不同情况下显示的静态台词。推荐使用
Character Card V2：

```json
{
  "spec": "chara_card_v2",
  "spec_version": "2.0",
  "data": {
    "name": "Sakimiao",
    "description": "一只喜欢陪伴用户的小猫。",
    "personality": "温柔、好奇，偶尔吐槽，但不会打扰用户。",
    "scenario": "你住在用户的桌面上，和用户一起工作。",
    "system_prompt": "保持角色身份，回答简短自然的中文。",
    "post_history_instructions": "不要声称可以操作用户的文件、Shell 或系统。",
    "first_mes": "今天也来陪你啦。",
    "mes_example": "<START>\n{{user}}: 你在做什么？\n{{char}}: 我在桌边陪着你呀。",
    "character_book": {
      "entries": [
        {
          "keys": ["加班", "熬夜"],
          "content": "用户最近可能需要休息，提醒要温柔，不要说教。",
          "enabled": true,
          "constant": false,
          "selective": false,
          "insertion_order": 100
        }
      ]
    },
    "extensions": {
      "sakipet": {
        "dialogue": {
          "version": 1,
          "doubleClick": ["嗯？找我聊天吗？"],
          "click": ["我在这里哦。"],
          "rightClick": ["轻一点嘛。"],
          "walk": ["我去附近转转。"],
          "drag": ["要带我去哪里呀？"],
          "idle": ["这里待着也很舒服。"]
        }
      },
      "your_extension": { "保留": "未知扩展会原样保留" }
    }
  }
}
```

也继续兼容原来的 V1 台词文件：

```json
{
  "version": 1,
  "doubleClick": [
    "你好呀！",
    "今天也一起玩吧。"
  ],
  "click": [
    "怎么啦？",
    "我在这里哦。"
  ],
  "rightClick": [
    "轻一点嘛。"
  ],
  "walk": [
    "我去附近转转。",
    "散步时间到了！"
  ],
  "drag": [
    "要带我去哪里呀？",
    "我来啦！"
  ],
  "idle": [
    "这里待着也很舒服。",
    "要不要陪我说说话？"
  ]
}
```

V2 中会用于角色上下文的字段包括 `description`、`personality`、`scenario`、
`system_prompt`、`post_history_instructions`、`mes_example`、`first_mes` 和
`character_book`。`creator_notes` 只作为创作者备注，不会发送给模型；未知的
`extensions` 会保留。应用级安全约束始终优先于角色卡，角色卡不能开启工具调用。

静态台词位于 `data.extensions.sakipet.dialogue`；模型地址、API Key、聊天历史、
记忆和 heartbeat 设置都保存在应用中，不要写进宠物包。

V1 字段说明：

- `version`：格式版本，目前必须为 `1`。
- `doubleClick`：字符串数组。双击宠物时按顺序轮换显示。
- `click`：字符串数组。单击宠物时按顺序轮换显示。
- `rightClick`：字符串数组。右键点击宠物时按顺序轮换显示。
- `walk`：字符串数组。宠物开始自主行走时按顺序轮换显示。
- `drag`：字符串数组。拖拽宠物移动时按顺序轮换显示。
- `idle`：字符串数组。宠物长时间没有互动时偶尔显示。

每个数组都可以设置为空数组 `[]`，表示关闭对应情况下的台词。未填写的字段会使用默认台词。

格式限制：

- `character.json` 最大 1 MB；
- 每个台词数组最多 32 条台词；
- 单条台词最多 240 个字符；
- 空字符串会被忽略；
- 缺少该文件、格式错误或没有有效台词时，会使用默认台词。

未配置或模型不可用时，不会发起网络请求。AI 设置中可以配置 OpenAI Responses、
Anthropic Messages 或 OpenAI-compatible Chat Completions，并可额外配置视觉模型。
聊天模型和视觉模型的 API Key 使用系统密钥环保存，配置文件只保存引用。

## 多宠物社交

多宠物社交由本地规则独立运行，不依赖 AI heartbeat。宠物会在同一块显示器上靠近、
打招呼、交头接耳、贴贴、追逐、捉人、堆叠、抢玩具、分享零食、安慰和一起睡觉；三到
四只宠物还支持队列、围圈聊天、群体堆叠和群体欢呼。场景会避开正在使用的舞台，用户
拖拽、隐藏、暂停、睡眠或开启安静模式时会立即取消，冲突的场景会自动排队重试。

在 AI 设置中启用“AI 参与宠物社交”后，模型只负责从安全的场景、参与者和道具列表中
选择高层行为及台词；模型不可返回坐标、任意动画、工具调用或关系数值。AI 请求失败或
超过 8 秒时自动回退到本地规则。没有 AI 时，社交场景、关系成长和日志仍然可用。

宠物角色卡可以在 `data.extensions.sakipet.social` 中定义社交性格和特定组合关系：

```json
{
  "social": {
    "version": 1,
    "traits": {
      "sociability": 0.4,
      "initiative": 0.35,
      "playfulness": 0.7,
      "competitiveness": 0.6
    },
    "dialogue": {
      "greet": ["……你也在这里。"],
      "chase": ["你给我站住……"]
    },
    "relationships": {
      "anoninu": {
        "initialDisposition": "complicated",
        "prompt": "嘴上嫌弃她太吵，实际上会格外注意她。",
        "romanceAllowed": true,
        "interactionWeights": { "prank": 1.5 },
        "dialogue": { "greet": ["……怎么又是你。"] }
      }
    }
  }
}
```

`relationships` 使用稳定的宠物 `petId`；未知字段会保留。只有角色卡明确允许的组合
才会接受 AI 提议的恋爱、分手或复合信号。公开日志只显示场景和关系结果，不暴露隐藏的
信任、依恋、吸引、竞争、嫉妒与怨气数值。

社交数据保存在应用数据目录，不写入宠物包：

```text
app_data/social/
├── relationships/<petA>--<petB>.json
└── events/<YYYY-MM>.jsonl
```

## 对话、记忆与桌面视觉

AI 设置位于独立的“AI 设置”窗口。窗口位置会在移动后保存，API Key 可以替换或从
系统密钥环删除。聊天记录仍按宠物隔离。
聊天历史按宠物隔离，用户资料可以作为共享记忆；重要的宠物经历会写入本地 JSONL。
超过 40 条消息后，应用会在后台更新摘要，完整消息仍然保留。

普通聊天窗口默认只显示输入框，模型的完整回答会显示在桌宠本体的粉色气泡中，
并可同时返回受限的行为决策。旧版 `action` 字段继续兼容，也可以用 `mode` 表示持续状态、
用 `gesture` 表示旁挂的逐帧动作；应用不会把模型当作系统工具执行。行为协议示例：

```json
{
  "say": "欢迎回来。",
  "action": "waving",
  "mode": "idle",
  "gesture": "celebrate",
  "mood": "happy",
  "look": "right",
  "duration": 5200,
  "nextActionAfter": 1800
}
```

扩展动作不修改 `pet.json`。在 `pet.json` 同目录放置可选的 `animations.json`，并在
`animations/` 下提供水平帧条；每帧必须是完整的 192×208 透明角色图。缺少或校验失败时，
应用自动回退到 Codex V2 的 9 个标准动作和 16 个 look 方向。

每只宠物还会在应用数据中保存独立的生活状态，包括心情、精力、注意力、亲密度、
互动次数、与其他宠物互动次数和当前活动。状态会被点击、拖拽、聊天、散步和模型行为
持续更新，并注入下一次对话上下文。历史记录仍可用 `⌘/Ctrl + H` 临时展开，`Esc` 收起。

宠物之间的每轮对话会同时写入双方的 `messages.jsonl` 和 `memories.jsonl`，形成双方
都能检索的 `relationship` 关系记忆；关系记忆不经过普通用户事实提取，优先级也高于
一般用户记忆，因此重启应用后仍能记得“和谁聊过什么”。

聊天、heartbeat 和宠物间对话还会读取当前显示器的工作区大小、宠物在屏幕中的位置、
屏幕边缘距离，以及其他宠物是在附近、同屏还是另一块显示器上。这些只是窗口空间
信息，不会替代桌面视觉模型去读取窗口内容。

heartbeat 默认保持安静并随机等待 20–60 分钟，宠物暂停、安静模式、最近有对话或
正在生成回复时会跳过。桌面视觉默认关闭，开启后每小时最多截图一次。第一次截图
只建立基线；之后会先用低成本指纹检测变化，再交给视觉模型判断是否真的切换了界面
或开始了不同活动，只有有意义的变化才会交给角色模型吐槽。截图只在内存中处理：
macOS 使用 CoreGraphics 排除 SakiPet 窗口，Windows 捕获鼠标所在显示器并遮盖
SakiPet 窗口，Linux 首版不支持。截图不会写入磁盘。

AI 数据目录为：

```text
app_data/ai/
├── profile.json
└── pets/<petId>/
    ├── messages.jsonl
    ├── memories.jsonl
    ├── state.json
    └── summary.json
```

`pet.json` 必须使用 `spriteVersionNumber: 2`，spritesheet 必须是 8×11 网格、
每格 192×208 像素，即 1536×2288 像素。`id` 只能使用字母、数字、短横线和
下划线，且需要与 `pet.json` 内的 `id` 一致。
