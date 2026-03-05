import { ClientSDK } from "../lib/sdks.js";
import { BenefitGrants } from "./benefitgrants.js";
import { Downloadables } from "./downloadables.js";
import { PolarCustomers } from "./polarcustomers.js";
import { PolarLicenseKeys } from "./polarlicensekeys.js";
import { PolarOrders } from "./polarorders.js";
import { PolarOrganizations } from "./polarorganizations.js";
import { PolarSubscriptions } from "./polarsubscriptions.js";
export declare class CustomerPortal extends ClientSDK {
    private _benefitGrants?;
    get benefitGrants(): BenefitGrants;
    private _customers?;
    get customers(): PolarCustomers;
    private _downloadables?;
    get downloadables(): Downloadables;
    private _licenseKeys?;
    get licenseKeys(): PolarLicenseKeys;
    private _orders?;
    get orders(): PolarOrders;
    private _organizations?;
    get organizations(): PolarOrganizations;
    private _subscriptions?;
    get subscriptions(): PolarSubscriptions;
}
//# sourceMappingURL=customerportal.d.ts.map