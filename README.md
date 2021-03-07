# v2ray-monitor

使用rust写的v2ray节点故障自动切换应用

## 功能计划

### 已实现

- 自动响应切换

从0.4版本开始支持监控流量自动切换，之前的版本都主动发送tcp查询网络问题，在网络响应时间上有了巨大的提升，响应切换生效通常在秒级内，cpu利用率在可控范围内，同时因为主动发送流量出现问题：[系统检测到您的计算机网络发出了异常流量](https://support.google.com/websearch/answer/86640?hl=zh-Hans#zippy=%2C%E6%88%91%E6%B2%A1%E7%9C%8B%E5%88%B0%E4%BA%BA%E6%9C%BA%E8%AF%86%E5%88%AB%E7%B3%BB%E7%BB%9F%E5%9B%BE%E7%89%87)

- 定时任务更新节点信息与tcp ping数据

可以通过配置节点数据自动更新

### todo

- 结点切换选择权重算法

当前还有一个问题是结点失效后可能被再次切换到，这涉及到权重计算。当前版本0.5仅简单的计算在固定时间内切换则+1/2/3，当更新节点数据时清除之前切换数据，不能很好的实现节点选择，但是基本可用

- 测试环境

一个问题是没有稳定的测试环境，只能依赖实际的设备测试，虽然rust减少了许多错误，但是不能保证细节算法的问题，如果是java写的，没有稳定的测试都不敢在实际设备使用出错的可能太多（哈哈）

基本思路是使用docker模拟设备环境测试

- 跨平台支持

当前仅支持在linux arm测试过，而且仅支持`host ssh -> openwrt`这种模式测试可用，虽然支持linux本机，但是没有做过测试，实际不可用。当前不支持windows设备，这个需求支持可能要等很久。。。

- 内存占用优化

目前内存上感觉占用有点高，在长时间运行后可能达到`35M`的内存占用，这还不算v2ray tcpping时开的v2ray进程，对于低内存设备很不友好。如果开`4`v2ray并发ping可能需要`100..150M`的内存占用
