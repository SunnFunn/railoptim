SELECT 
    GU.ClaimNumberInt, 
    GU.LoaderFromName, 
    GU.LoaderFromOKPO,
    GU.StationFromName, 
    GU.StationFromCode6, 
    GU.StationToName, 
    GU.StationToCode6, 
    GU.SendKindName, 
    GU.FrETSNG, 
    GU.FrETSNGCode6, 
    GU.FinishDate, 
    GU.CarCount AS TotalCars, 
    SUM(CASE 
            WHEN POD.PodDate >= DATEADD(day, 1, CAST(GETDATE() AS DATE))
             AND POD.PodDate <= DATEADD(day, 5, CAST(GETDATE() AS DATE))
            THEN POD.CarCount 
            ELSE 0 
        END) AS [1-5 сутки], 
    SUM(CASE 
            WHEN POD.PodDate >= DATEADD(day, 6, CAST(GETDATE() AS DATE))
             AND POD.PodDate <= DATEADD(day, 8, CAST(GETDATE() AS DATE))
            THEN POD.CarCount 
            ELSE 0 
        END) AS [6-8 сутки], 
    SUM(CASE 
            WHEN POD.PodDate >= DATEADD(day, 9, CAST(GETDATE() AS DATE))
             AND POD.PodDate <= DATEADD(day, 10, CAST(GETDATE() AS DATE))
            THEN POD.CarCount 
            ELSE 0 
        END) AS [9-10 сутки], 
    SUM(CASE 
            WHEN POD.PodDate >= DATEADD(day, 11, CAST(GETDATE() AS DATE))
             AND POD.PodDate <= DATEADD(day, 15, CAST(GETDATE() AS DATE))
            THEN POD.CarCount 
            ELSE 0 
        END) AS [11-15 сутки]
FROM vClaimGU12 GU (NOLOCK) 
JOIN vClaimGu12OtprGraphPod POD (NOLOCK) ON POD.ClaimGu12OtprId = GU.Id
WHERE 
    POD.PodDate >= DATEADD(day, 1, CAST(GETDATE() AS DATE)) 
    AND POD.PodDate <= DATEADD(day, 15, CAST(GETDATE() AS DATE)) 
    AND GU.IsVisible = 1 
    AND GU.StateName IN ('Согласована', 'Согласована частично', 'Согласована 53ф', 'Согласована с изменениями 53ф') 
    AND GU.FrETSNGCode6 LIKE '[05]%' -- Код начинается с 0 или 5 
GROUP BY 
    GU.ClaimNumberInt, 
    GU.LoaderFromName, 
    GU.LoaderFromOKPO,
    GU.StationFromName, 
    GU.StationFromCode6, 
    GU.StationToName, 
    GU.StationToCode6, 
    GU.SendKindName, 
    GU.FrETSNG, 
    GU.FrETSNGCode6, 
    GU.FinishDate, 
    GU.CarCount;
