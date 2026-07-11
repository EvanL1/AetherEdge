# Aether Product Library

AetherEMS 产品模板库，定义储能系统中各类设备的测点、动作点和属性。

## 产品层级

```
Station (场站)
├── ESS (储能系统)
│   ├── Battery (电池)
│   └── PCS (变流器)
├── Generator (发电)
│   ├── Diesel (柴油机)
│   └── PV DCDC (光伏)
├── Env (环境监控)
└── Load (负载)
```

## JSON 结构

```json
{
  "name": "Battery",
  "pName": "ESS",
  "P": [{"id": 1, "name": "Max Power", "unit": "kw", "type": "number"}],
  "M": [{"id": 1, "name": "SOC", "unit": "%", "type": "number"}],
  "A": [{"id": 1, "name": "Start", "unit": "", "type": "string"}]
}
```

| 字段 | 说明 |
|------|------|
| `name` | 产品名称（唯一标识） |
| `pName` | 父产品名称（用于层级） |
| `P` | 属性定义（Property） |
| `M` | 测量点定义（Measurement） |
| `A` | 动作点定义（Action） |

## 使用方式

### Rust 项目

```rust
// 方式 1：Git Submodule
git submodule add https://github.com/your-org/aether-product-lib products

// 方式 2：编译时嵌入
include_str!("products/Battery.json")
```

### Java 项目

```bash
# Git Submodule
git submodule add https://github.com/your-org/aether-product-lib src/main/resources/products
```

```java
// 读取 JSON
ObjectMapper mapper = new ObjectMapper();
Product product = mapper.readValue(
    getClass().getResourceAsStream("/products/Battery.json"),
    Product.class
);
```

### 校验

使用 `schema/product.schema.json` 校验产品定义：

```bash
# 使用 ajv-cli
npx ajv validate -s schema/product.schema.json -d "products/*.json"
```

## License

MIT
