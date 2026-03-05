import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerPortalCustomer } from "../models/components/customerportalcustomer.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalCustomersGetSecurity } from "../models/operations/customerportalcustomersget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Customer
 *
 * @remarks
 * Get authenticated customer.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalCustomersGet(client: PolarCore, security: CustomerPortalCustomersGetSecurity, options?: RequestOptions): APIPromise<Result<CustomerPortalCustomer, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalCustomersGet.d.ts.map