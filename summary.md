# 总结

在自动切换的功能实现上还有更好的实现：根据网卡流量自动检查发送到外网ip流量是否存在响应。
如果在规定时间内没有响应就可以判断网络出现问题，就不需要主动发送google请求检测网络。这种方式还可实现更多的功能如多个外网ip监控、不需要空闲时间检查

## 问题

### 在异步中如何使用static lazy初始化

在测试时需要使用tcp ping后的数据，这个测试耗时多只要执行一次就够了

#### 方法

static 结合[once_cell库](https://docs.rs/crate/once_cell/1.6.0/source/)在async函数中定义static field，多次调用该函数将只会执行一次初始化

```rust
async fn get_node_stats() -> Result<Vec<(Node, TcpPingStatistic)>> {
    static NODE_STATS: OnceCell<Mutex<Vec<(Node, TcpPingStatistic)>>> = OnceCell::new();
    // once_cell同步初始化
    let mut ns = NODE_STATS.get_or_init(|| Mutex::new(vec![])).lock();
    if !ns.is_empty() {
        return Ok(ns.to_vec());
    }
    // 初始化
    let nodes = get_nodes().await?;
    *ns = nodes;
    Ok(ns.to_vec())
}
```

### future cannot be sent between threads safely for self

```
future cannot be sent between threads safely
within `impl futures::Future`, the trait `std::marker::Send` is not implemented for `*mut ()`rustc
lib.rs(1, 1): required by a bound in this
spawn.rs(129, 21): required by this bound in `tokio::spawn`
switch.rs(58, 14): future is not `Send` as this value is used across an await
```

```rust
pub struct A {
    vals: Arc<Mutex<HashSet<Val>>>,
}

fn handle(val: &HashSet<Val>) -> Result<bool> {

    Ok(true)
}
struct Val;


impl A {
    pub fn new() -> Self {
        Self {
            vals: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn run(&self) -> Result<()> {
        if let Ok(is_forword) = handle( &self.vals.clone().lock()) {
            // error: future is not `Send` as this value is used across an await
             self.call_async().await;
            // self.call_sync();
        }
        Ok(())
    }

    fn call_sync(&self) {

    }

    async fn call_async(&self) {
    }
}

async fn test() {
    tokio::spawn(async move {
        A::new()
            .run()
            .await
            .unwrap()
    });
}
```

参考：

* [how to init with async method](https://github.com/matklad/once_cell/issues/108)
* [alternative to using 'await' with lazy_static! macro in rust?](https://stackoverflow.com/questions/62351945/alternative-to-using-await-with-lazy-static-macro-in-rust)

### 在异步中RAII

> To be honest, the idiomatic way is to design your code such that you don't need async drop

参考：

* [Asynchronous Destructors](https://internals.rust-lang.org/t/asynchronous-destructors/11127)
* [What is the idiomatic way of cleaning up async resources in rust?](https://users.rust-lang.org/t/what-is-the-idiomatic-way-of-cleaning-up-async-resources-in-rust/48878)
* [How do I implement an async Drop in Rust?](https://stackoverflow.com/questions/59782278/how-do-i-implement-an-async-drop-in-rust)
* [scopeguard](https://github.com/bluss/scopeguard)

可能的解决方法：

* [A practical introduction to async programming in Rust](http://jamesmcm.github.io/blog/2020/05/06/a-practical-introduction-to-async-programming-in-rust/)
* [Atomic renames and asynchronous destructors #16](https://github.com/jamesmcm/s3rename/issues/16)
