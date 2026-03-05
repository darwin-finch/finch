import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalBenefitGrantsListRequest, CustomerPortalBenefitGrantsListResponse, CustomerPortalBenefitGrantsListSecurity } from "../models/operations/customerportalbenefitgrantslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Benefit Grants
 *
 * @remarks
 * List benefits grants of the authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalBenefitGrantsList(client: PolarCore, security: CustomerPortalBenefitGrantsListSecurity, request: CustomerPortalBenefitGrantsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<CustomerPortalBenefitGrantsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=customerPortalBenefitGrantsList.d.ts.map