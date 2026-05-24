import pandas as pd


# df = pd.read_parquet("stations_nsi_raw.parquet")
# df = pd.read_parquet("sbin_esr_index.parquet")
df = pd.read_parquet("osm_esr_index.parquet")
df.to_excel("test.xlsx", index=False)
print("файл сохранен")