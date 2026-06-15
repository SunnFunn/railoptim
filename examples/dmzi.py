
import requests

def fetch_dmzi(path = path, driver = driver, server = server, database = database, token=token, url=url):
    header = {'Authorization': 'Bearer {}'.format(token)}
    
    params_dmzi = {
        "DateBegin": "2026-06-10",
        "DateEnd": "2026-06-16", # 7 дней доступный горизонт
        "RailWayId": "",
        "CarKindId": "20", # зерновозы
        "NormativeId": "6", # заадресовка
        }

    try:
        response = requests.get(url + "GetDMZIData", headers=header, params=params_dmzi)
        dmzi_list = response.json()
        dmzi_list_cleared = [ele for ele in dmzi_list if ele["NormativType"] == "Ostatok"]
        # поля/ключи словарей в списке dmzi_list_cleared с примерами value (чтобы легче было понять тип данных):
        # {
        #     "DateDMZIRequest": 2026-06-10T10:35:00.153,
        #     "DMZIRailWayGroup": "МСК/ЗНВ", # 3-х буквенное имя  ж-д дороги, на котрое наложено ограничение ДМЗИ / тип подвижного состава (у нас зерновозы)
        #     "NormativName": "Заадресовка", # на что накладываентся инфраструктурное ограничение (в нашем случае на подсыл порожних вагонов на дорогу)
        #     "DateOfNormativ": 2026-06-13T00:00:00,
        #     "NormativType": "ostatok",
        #     "Normativ": 25, # integer number норматив на подсыл порожних вагонов на дорогу (не больше, верхняя граница)
        #     "IdQueue": 44299342, # meta data 
        # }

        dmzi_df = pd.DataFrame.from_dict(dmzi_list_cleared)
        dmzi_df.to_excel("dmzi.xlsx")
    except Exception as e:
        print(f"Error: {e}")
        print(response.status_code, response.json())
    finally:
        print(len(response.json()))
        print(response.status_code)
