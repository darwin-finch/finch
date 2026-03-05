import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { CustomerOrganization } from "../models/components/customerorganization.js";
import { CustomerPortalOrganizationsGetRequest } from "../models/operations/customerportalorganizationsget.js";
export declare class PolarOrganizations extends ClientSDK {
    /**
     * Get Organization
     *
     * @remarks
     * Get a customer portal's organization by slug.
     */
    get(request: CustomerPortalOrganizationsGetRequest, options?: RequestOptions): Promise<CustomerOrganization>;
}
//# sourceMappingURL=polarorganizations.d.ts.map