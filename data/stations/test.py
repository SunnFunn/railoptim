import pandas as pd


df = pd.read_parquet("stations_nsi_raw.parquet")
df.to_excel("test.xlsx", index=False)
print("файл сохранен")