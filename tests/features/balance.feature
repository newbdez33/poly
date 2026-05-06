Feature: 余额展示
  作为机器人主人，我希望 TUI 启动后能立刻看到当前 USDC 余额

  Scenario: 缓存里已有余额，启动即显示
    Given Redis 缓存里有余额 "100.00" USDC
    When  我启动 TUI 主循环
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $100.00"

  Scenario: 缓存为空，CLOB 返回 50.00
    Given Redis 缓存为空
    And   CLOB 返回余额 "50.00" USDC
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $50.00"

  Scenario: CLOB 失败，仍显示旧缓存
    Given Redis 缓存里有余额 "200.00" USDC
    And   CLOB 调用会失败
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上仍显示 "USDC: $200.00"

  Scenario: 按 q 触发关闭
    Given Redis 缓存为空
    When  我启动 TUI 主循环
    And   按下 "q" 键
    Then  应用进入退出状态
