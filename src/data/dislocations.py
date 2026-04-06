import pymssql
import json
import redis


# функция получения данных о текущей дислокации вагонов Русагротранс
# ----------------------------------------------------------------------------------------------------------------------------------------------------------------
def fetch_dislocations(conndict, host, port, db, password, extime):
    r = redis.Redis(host=host, port=port, db=db, password=password)

    # из БД Redis необходимо получить номера вагонов и только по этим номерам вагонов отобрать из MSSQL сервера данные для узлов SupplyNode в преиод 2-10 сутки

    ########### ИНСТРУКЦИИ по подключению к редис ##########################
    # при подключении необходимо использовать следующие переменные окружения (только передеалй для питона):
    # // выгружаем переменные окружения приложения
    # let host = std::env::var("REDIS_SUPPLY_HOST").unwrap_or_else(|_| "localhost".to_string());
    # let port = std::env::var("REDIS_SUPPLY_PORT")
    #     .unwrap_or_else(|_| "6380".to_string())
    #     .parse::<u16>()
    #     .expect("Port must be a number");
    # let db = std::env::var("REDIS_SUPPLY_DB")
    #     .unwrap_or_else(|_| "0".to_string())
    #     .parse::<i64>()
    #     .expect("DB must be a number");

    # // Оборачиваем пароль в SecretString сразу при получении
    # let password = std::env::var("REDIS_SUPPLY_PASS")
    #     .ok()
    #     .map(SecretString::from);


    conn = pymssql.connect(server=conndict['server'],
                           user=(conndict['domain'] + conndict['user']),
                           password=conndict['password'],
                           database=conndict['database'])
    
    cursor = conn.cursor()

    stmt= \
    f'''
    SELECT
        DP.CarNumber,
        DP.FromRailWayPart, DP.FromRailWay, DP.StationFromName, DP.StationFromCode,
        DP.ToRailWayPart, DP.ToRailWay, DP.StationToName, DP.StationToCode,
        DP.CarCapacity, DP.CarBodyVolume, DP.CarSize, DP.IsCarRepair, DP.CarNextRepairDays,
        DP.FrETSNGCode, DP.FrETSNGName, FR.Code6, DP.PrevFrETSNGName,
        DP.GRPOName
    FROM DislocationPreview DP (NOLOCK)
        JOIN NSI.FrETSNG FR ON FR.Name = DP.PrevFrETSNGName
        JOIN dynamic.CarComment CC (NOLOCK) ON CC.CarId = DP.CarId
        LEFT JOIN NSI_ETRAN.ShipmentGoal SG (NOLOCK) ON DP.TranspPurpose = SG.NAME
    WHERE DP.BelongType  IN ('Арендованный','В лизинге', 'Собственный')
        AND CarKindName = 'Зерновозы';
    '''
        
    cursor.execute(stmt)
    ########## ИНСТРУКЦИИ узла SupplyNode для периода 2-10 сутки #############
    # Поле CarKind узла для всех вагонов из периода 2-10 сутки --> Free
    # Поле status узла для всех вагонов из поля DP.GRPOName
    # поля etsng, etsng_name, prev_etsng, prev_etsng_name из полей DP.FrETSNGCode, DP.FrETSNGName, FR.Code6, DP.PrevFrETSNGName
    # Поле car_type: Если DP.CarBodyVolume < 108.0 и DP.CarCapacity < 70.0 ---> Прочие; Если DP.CarBodyVolume >= 108.0 и DP.CarCapacity < 75.0 ---> БК;
    # Если DP.CarBodyVolume < 108.0 и DP.CarCapacity >= 75.0 ---> Т; Если DP.CarBodyVolume >= 108.0 и DP.CarCapacity >= 75.0 ---> БКТ;

    cursor.close()
    conn.close()
